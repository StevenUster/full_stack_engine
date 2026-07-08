//! Dynamic SELECT/UPDATE/DELETE builders — entered via the derive-generated
//! `Table::find()`, `Table::update_set()` and `Table::delete_where()`.
//! Same operator vocabulary as the checked `find!` macro, so moving a query
//! between the two is mechanical. No `_opt` operators here: compose optional
//! filters with plain `if`.

use std::marker::PhantomData;

use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Sqlite};

use crate::Page;
use crate::column::{Bindable, BindFn, Col, Cond, Order, bind_fn};

pub struct SelectBuilder<T> {
    table: &'static str,
    conds: Vec<Cond>,
    orders: Vec<String>,
    limit: Option<i64>,
    offset: Option<i64>,
    _marker: PhantomData<fn() -> T>,
}

impl<T> SelectBuilder<T>
where
    T: Send + Unpin + for<'r> sqlx::FromRow<'r, SqliteRow>,
{
    pub fn new(table: &'static str) -> Self {
        Self {
            table,
            conds: Vec::new(),
            orders: Vec::new(),
            limit: None,
            offset: None,
            _marker: PhantomData,
        }
    }

    /// Add a condition; multiple `filter` calls are ANDed.
    pub fn filter(mut self, cond: Cond) -> Self {
        self.conds.push(cond);
        self
    }

    pub fn order_by(mut self, order: Order) -> Self {
        self.orders.push(order.0);
        self
    }

    /// Negative values are clamped to 0: SQLite reads a negative LIMIT as
    /// "unlimited", and an unvalidated value must not turn a bounded query
    /// into a full-table read.
    pub fn limit(mut self, limit: i64) -> Self {
        self.limit = Some(limit.max(0));
        self
    }

    pub fn offset(mut self, offset: i64) -> Self {
        self.offset = Some(offset.max(0));
        self
    }

    pub async fn fetch_all(self, db: impl sqlx::SqliteExecutor<'_>) -> sqlx::Result<Vec<T>> {
        let mut qb = self.select_query(self.limit, self.offset);
        qb.build_query_as::<T>().fetch_all(db).await
    }

    pub async fn fetch_optional(
        self,
        db: impl sqlx::SqliteExecutor<'_>,
    ) -> sqlx::Result<Option<T>> {
        let mut qb = self.select_query(Some(self.limit.unwrap_or(1)), self.offset);
        qb.build_query_as::<T>().fetch_optional(db).await
    }

    pub async fn fetch_one(self, db: impl sqlx::SqliteExecutor<'_>) -> sqlx::Result<T> {
        let mut qb = self.select_query(Some(self.limit.unwrap_or(1)), self.offset);
        qb.build_query_as::<T>().fetch_one(db).await
    }

    pub async fn count(self, db: impl sqlx::SqliteExecutor<'_>) -> sqlx::Result<i64> {
        let mut qb = QueryBuilder::new(format!("SELECT COUNT(*) FROM {}", self.table));
        push_where(&self.conds, &mut qb);
        qb.build_query_scalar::<i64>().fetch_one(db).await
    }

    /// COUNT + page in one call over the same filter. Runs two queries, so
    /// the executor must be reusable — pass a pool reference.
    pub async fn fetch_page(
        self,
        db: impl sqlx::SqliteExecutor<'_> + Copy,
        page: i64,
        per_page: i64,
    ) -> sqlx::Result<Page<T>> {
        let page = page.max(1);
        // SQLite reads a negative LIMIT as "unlimited" — a hostile per_page
        // must not dump the whole table.
        let per_page = per_page.max(1);

        let mut count_qb = QueryBuilder::new(format!("SELECT COUNT(*) FROM {}", self.table));
        push_where(&self.conds, &mut count_qb);
        let total = count_qb.build_query_scalar::<i64>().fetch_one(db).await?;

        let mut qb = self.select_query(Some(per_page), Some((page - 1) * per_page));
        let rows = qb.build_query_as::<T>().fetch_all(db).await?;
        Ok(Page { rows, total })
    }

    fn select_query(
        &self,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> QueryBuilder<'static, Sqlite> {
        let mut qb = QueryBuilder::new(format!("SELECT * FROM {}", self.table));
        push_where(&self.conds, &mut qb);
        if !self.orders.is_empty() {
            qb.push(format!(" ORDER BY {}", self.orders.join(", ")));
        }
        match (limit, offset) {
            (Some(limit), Some(offset)) => {
                qb.push(format!(" LIMIT {limit} OFFSET {offset}"));
            }
            (Some(limit), None) => {
                qb.push(format!(" LIMIT {limit}"));
            }
            (None, Some(offset)) => {
                qb.push(format!(" LIMIT -1 OFFSET {offset}"));
            }
            (None, None) => {}
        }
        qb
    }
}

pub struct UpdateBuilder {
    table: &'static str,
    sets: Vec<(&'static str, BindFn)>,
    conds: Vec<Cond>,
}

impl UpdateBuilder {
    pub fn new(table: &'static str) -> Self {
        Self { table, sets: Vec::new(), conds: Vec::new() }
    }

    /// `set(Product::PRICE, 4.5)` — the column token keeps the value
    /// Rust-type-checked. json columns have no `Bindable` type, so they
    /// cannot be set here; use the checked `update!` or a full-row update.
    pub fn set<T: Bindable>(mut self, column: Col<T>, value: impl Into<T>) -> Self {
        self.sets.push((column.name(), bind_fn(value.into())));
        self
    }

    /// Add a condition; multiple `filter` calls are ANDed. No filter means
    /// every row, as in SQL.
    pub fn filter(mut self, cond: Cond) -> Self {
        self.conds.push(cond);
        self
    }

    /// Returns the number of updated rows.
    pub async fn execute(self, db: impl sqlx::SqliteExecutor<'_>) -> sqlx::Result<u64> {
        if self.sets.is_empty() {
            return Err(sqlx::Error::Protocol(
                "UpdateBuilder needs at least one set()".into(),
            ));
        }
        let mut qb = QueryBuilder::new(format!("UPDATE {} SET ", self.table));
        for (i, (name, bind)) in self.sets.iter().enumerate() {
            if i > 0 {
                qb.push(", ");
            }
            qb.push(format!("{name} = "));
            bind(&mut qb);
        }
        push_where(&self.conds, &mut qb);
        let result = qb.build().execute(db).await?;
        Ok(result.rows_affected())
    }
}

pub struct DeleteBuilder {
    table: &'static str,
    conds: Vec<Cond>,
}

impl DeleteBuilder {
    pub fn new(table: &'static str) -> Self {
        Self { table, conds: Vec::new() }
    }

    /// Add a condition; multiple `filter` calls are ANDed. No filter means
    /// every row, as in SQL.
    pub fn filter(mut self, cond: Cond) -> Self {
        self.conds.push(cond);
        self
    }

    /// Returns the number of deleted rows.
    pub async fn execute(self, db: impl sqlx::SqliteExecutor<'_>) -> sqlx::Result<u64> {
        let mut qb = QueryBuilder::new(format!("DELETE FROM {}", self.table));
        push_where(&self.conds, &mut qb);
        let result = qb.build().execute(db).await?;
        Ok(result.rows_affected())
    }
}

fn push_where(conds: &[Cond], qb: &mut QueryBuilder<'_, Sqlite>) {
    for (i, cond) in conds.iter().enumerate() {
        qb.push(if i == 0 { " WHERE (" } else { " AND (" });
        cond.apply(qb);
        qb.push(")");
    }
}
