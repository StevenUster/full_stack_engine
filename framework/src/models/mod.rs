//! The runtime model registry — the core of struct-defined apps.
//!
//! `#[derive(Model)]` (from `full_stack_engine_macros`, re-exported in the
//! prelude) parses a `#[derive(Table)]` struct with the same fse-schema code
//! the ORM uses, validates the framework-owned `#[model(...)]`/`#[ui(...)]`
//! attributes at compile time, and submits a [`ModelRegistration`] here via
//! `inventory`. At boot the framework reads [`registered_models`] to mount
//! generic CRUD routes; the generic templates render lists and forms from the
//! same metadata. Models defined in dependency crates (modules) register
//! through the exact same path — linking the crate is enough.
//!
//! Nothing in this module validates dev input: everything expressible in the
//! attributes was already checked by the macro. This module only resolves
//! conventions (permission names, paths, default column sets) that need the
//! whole picture at runtime.

use fse_schema::{ColumnDef, SqlType, TableDef};
use std::sync::LazyLock;

pub mod form;
mod resource;
mod routes;

pub use routes::mount_all;

pub use resource::{
    Db, DbResult, FieldError, FormData, FormErrors, ListQuery, ListResult, ModelResource, and_opt,
};

// Re-exported under stable framework paths for the code `#[model]` emits.
pub use futures::future::BoxFuture;
pub use serde_json;

/// One `#[model]` struct as submitted by the macro: the fse-schema
/// `TableDef` serialized to JSON at macro-expansion time, the
/// const-constructed UI metadata, and the generated typed data access.
pub struct ModelRegistration {
    pub table_json: &'static str,
    pub ui: &'static UiModel,
    pub resource: &'static dyn ModelResource,
}

inventory::collect!(ModelRegistration);

/// Struct-level app configuration from `#[model(...)]`. `None`/`false`
/// everywhere means "all conventions" — resolved by [`ModelMeta`]'s
/// accessors, never read raw by handlers.
// The bools mirror independent bare attribute flags one-to-one — grouping
// them into state enums would only obscure that mapping.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy)]
pub struct UiModel {
    /// `permission = "products"` — base permission name.
    pub permission: Option<&'static str>,
    /// `path = "product-manager"` — base URL path segment.
    pub path: Option<&'static str>,
    /// `public_read` / `public_read = slug` — the unique column public
    /// read-only pages look rows up by.
    pub public_read: Option<&'static str>,
    /// `api` — expose JSON API endpoints.
    pub api: bool,
    /// `disabled` — metadata only, no generated routes.
    pub disabled: bool,
    pub no_create: bool,
    pub no_edit: bool,
    pub no_delete: bool,
    /// `title_field = name` — column shown as the row title.
    pub title_field: Option<&'static str>,
    /// One entry per database column, in declaration order (relations have
    /// no generated UI yet).
    pub fields: &'static [UiField],
    /// The columns generated create/edit forms expose, in order — computed
    /// by the macro (visible, editable, not the pk, not `default = now`,
    /// not json) and the exact set the generated `create`/`update` code
    /// binds, so the two can never drift.
    pub form_fields: &'static [&'static str],
}

/// Per-column UI configuration from `#[ui(...)]`, with widget defaults
/// resolved by the macro from the column type.
// Same as UiModel: one bool per independent #[ui(...)] flag.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy)]
pub struct UiField {
    pub name: &'static str,
    /// `#[ui(list)]` — show in the generated list table.
    pub list: bool,
    /// `#[ui(search)]` — the list search box matches this column.
    pub search: bool,
    /// `#[ui(filter)]` — offer a filter dropdown.
    pub filter: bool,
    /// `#[ui(readonly)]` — show but never edit in generated forms.
    pub readonly: bool,
    /// `#[ui(hidden)]` — never show in generated UI (json/blob columns
    /// default to hidden).
    pub hidden: bool,
    pub widget: UiWidget,
    /// For [`UiWidget::Select`]: yields the `DbEnum`'s stored values. A fn
    /// pointer because the derive cannot see the enum's variants — only the
    /// generated `VARIANTS` const on the enum type can.
    pub options: Option<fn() -> Vec<&'static str>>,
}

/// The form control a column renders as, defaulted from its SQL type
/// (`#[ui(textarea)]` upgrades a plain text column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiWidget {
    Text,
    Textarea,
    Number,
    Checkbox,
    DateTime,
    Select,
    Json,
}

impl UiWidget {
    /// The lowercase name templates switch on.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            UiWidget::Text => "text",
            UiWidget::Textarea => "textarea",
            UiWidget::Number => "number",
            UiWidget::Checkbox => "checkbox",
            UiWidget::DateTime => "datetime",
            UiWidget::Select => "select",
            UiWidget::Json => "json",
        }
    }
}

/// A registered model with its parsed table definition — what generic
/// handlers and templates work from.
pub struct ModelMeta {
    pub table: TableDef,
    pub ui: &'static UiModel,
    /// The generated typed data access for this model.
    pub resource: &'static dyn ModelResource,
}

static MODELS: LazyLock<Vec<ModelMeta>> = LazyLock::new(|| {
    let mut models: Vec<ModelMeta> = inventory::iter::<ModelRegistration>()
        .map(|reg| ModelMeta {
            table: serde_json::from_str(reg.table_json)
                .expect("table_json is written by the #[model] macro and always valid"),
            ui: reg.ui,
            resource: reg.resource,
        })
        .collect();
    models.sort_by(|a, b| a.table.name.cmp(&b.table.name));
    models
});

/// Every `#[derive(Model)]` struct linked into this binary, sorted by table
/// name.
///
/// # Panics
///
/// If two models resolve to the same table name (e.g. the same struct name in
/// an app and a module) — a conflict that cannot be seen at compile time, so
/// it fails fast here instead of behaving ambiguously.
#[must_use]
pub fn registered_models() -> &'static [ModelMeta] {
    let models = &*MODELS;
    if let Some(pair) = models.windows(2).find(|w| w[0].table.name == w[1].table.name) {
        panic!(
            "two models are registered for table `{}` (structs `{}` and `{}`) — rename one \
             or set #[orm(table = \"...\")]",
            pair[0].table.name, pair[0].table.struct_name, pair[1].table.struct_name
        );
    }
    models
}

/// Look up a registered model by SQL table name.
#[must_use]
pub fn model(table: &str) -> Option<&'static ModelMeta> {
    registered_models().iter().find(|m| m.table.name == table)
}

impl ModelMeta {
    /// The base permission name: `permission = "..."` or the table name.
    /// Generated read endpoints require `<base>.read`, mutations
    /// `<base>.write`.
    #[must_use]
    pub fn permission_base(&self) -> &str {
        self.ui.permission.unwrap_or(&self.table.name)
    }

    #[must_use]
    pub fn read_permission(&self) -> String {
        format!("{}.read", self.permission_base())
    }

    #[must_use]
    pub fn write_permission(&self) -> String {
        format!("{}.write", self.permission_base())
    }

    /// The base URL path of the generated admin UI, with a leading slash.
    /// An explicit `path = "product-manager"` mounts verbatim at
    /// `/product-manager`; the default is `/admin/{table}` so `public_read`
    /// pages can own the bare `/{table}` paths.
    #[must_use]
    pub fn base_path(&self) -> String {
        match self.ui.path {
            Some(p) => format!("/{p}"),
            None => format!("/admin/{}", self.table.name),
        }
    }

    #[must_use]
    pub fn ui_field(&self, name: &str) -> Option<&'static UiField> {
        self.ui.fields.iter().find(|f| f.name == name)
    }

    fn column_of(&self, field: &UiField) -> &ColumnDef {
        self.table
            .column(field.name)
            .expect("UiField names come from the same struct's columns")
    }

    /// Columns of the generated list table. Explicit `#[ui(list)]` flags win;
    /// with none present, every visible scalar column except the primary key
    /// is shown.
    #[must_use]
    pub fn list_columns(&self) -> Vec<&ColumnDef> {
        let explicit: Vec<&ColumnDef> = self
            .ui
            .fields
            .iter()
            .filter(|f| f.list)
            .map(|f| self.column_of(f))
            .collect();
        if !explicit.is_empty() {
            return explicit;
        }
        self.ui
            .fields
            .iter()
            .filter(|f| !f.hidden)
            .map(|f| self.column_of(f))
            .filter(|c| !c.primary_key)
            .collect()
    }

    /// Columns the list search box matches (`#[ui(search)]`).
    #[must_use]
    pub fn search_columns(&self) -> Vec<&ColumnDef> {
        self.ui
            .fields
            .iter()
            .filter(|f| f.search)
            .map(|f| self.column_of(f))
            .collect()
    }

    /// Columns offered as filter dropdowns (`#[ui(filter)]`).
    #[must_use]
    pub fn filter_columns(&self) -> Vec<&ColumnDef> {
        self.ui
            .fields
            .iter()
            .filter(|f| f.filter)
            .map(|f| self.column_of(f))
            .collect()
    }

    /// Columns generated create/edit forms expose — the macro-computed
    /// `form_fields` set (visible, editable, no pk, no `default = now`, no
    /// json), which is also exactly what the generated `create`/`update`
    /// code binds.
    ///
    /// # Panics
    ///
    /// Never in practice: `form_fields` is derived from the same struct's
    /// columns by the macro.
    #[must_use]
    pub fn form_columns(&self) -> Vec<&ColumnDef> {
        self.ui
            .form_fields
            .iter()
            .map(|name| {
                self.table
                    .column(name)
                    .expect("form_fields come from the same struct's columns")
            })
            .collect()
    }

    /// The column shown as a row's title: `title_field = ...`, else the
    /// first visible plain text column, else the primary key.
    ///
    /// # Panics
    ///
    /// Never in practice: `title_field` is validated against the columns by
    /// the derive at compile time.
    #[must_use]
    pub fn title_column(&self) -> &ColumnDef {
        if let Some(name) = self.ui.title_field {
            return self
                .table
                .column(name)
                .expect("title_field validated by the derive");
        }
        self.ui
            .fields
            .iter()
            .filter(|f| !f.hidden)
            .map(|f| self.column_of(f))
            .find(|c| c.ty == SqlType::Text && !c.is_enum && !c.json)
            .unwrap_or_else(|| self.table.primary_key()[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, ty: SqlType) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            rust_type: String::new(),
            ty,
            nullable: false,
            primary_key: name == "id",
            unique: false,
            json: false,
            is_enum: false,
            index: false,
            default: None,
            references: None,
            check_in: None,
            renamed_from: None,
        }
    }

    static PLAIN_UI: UiModel = UiModel {
        permission: None,
        path: None,
        public_read: None,
        api: false,
        disabled: false,
        no_create: false,
        no_edit: false,
        no_delete: false,
        title_field: None,
        fields: &[],
        form_fields: &[],
    };

    /// A resource that must never be called — meta-resolution tests only.
    struct NoResource;

    impl ModelResource for NoResource {
        fn list<'a>(&'a self, _: &'a Db, _: &'a ListQuery) -> BoxFuture<'a, DbResult<ListResult>> {
            unreachable!()
        }
        fn get<'a>(
            &'a self,
            _: &'a Db,
            _: i64,
        ) -> BoxFuture<'a, DbResult<Option<serde_json::Value>>> {
            unreachable!()
        }
        fn get_by_public<'a>(
            &'a self,
            _: &'a Db,
            _: &'a str,
        ) -> BoxFuture<'a, DbResult<Option<serde_json::Value>>> {
            unreachable!()
        }
        fn create<'a>(
            &'a self,
            _: &'a Db,
            _: &'a FormData,
        ) -> BoxFuture<'a, DbResult<Result<i64, FormErrors>>> {
            unreachable!()
        }
        fn update<'a>(
            &'a self,
            _: &'a Db,
            _: i64,
            _: &'a FormData,
        ) -> BoxFuture<'a, DbResult<Result<(), FormErrors>>> {
            unreachable!()
        }
        fn delete<'a>(&'a self, _: &'a Db, _: i64) -> BoxFuture<'a, DbResult<u64>> {
            unreachable!()
        }
    }

    fn meta(columns: Vec<ColumnDef>, fields: &'static [UiField]) -> ModelMeta {
        meta_with(columns, fields, &[])
    }

    fn meta_with(
        columns: Vec<ColumnDef>,
        fields: &'static [UiField],
        form_fields: &'static [&'static str],
    ) -> ModelMeta {
        let ui: &'static UiModel = Box::leak(Box::new(UiModel {
            fields,
            form_fields,
            ..PLAIN_UI
        }));
        ModelMeta {
            table: TableDef {
                name: "notes".into(),
                struct_name: "Note".into(),
                columns,
                relations: Vec::new(),
                composite_uniques: Vec::new(),
                composite_indexes: Vec::new(),
            },
            ui,
            resource: &NoResource,
        }
    }

    #[test]
    fn conventions_resolve_from_table_name() {
        static FIELDS: [UiField; 2] = [ui_field_const("id"), ui_field_const("title")];
        let m = meta(
            vec![column("id", SqlType::Integer), column("title", SqlType::Text)],
            &FIELDS,
        );
        assert_eq!(m.permission_base(), "notes");
        assert_eq!(m.read_permission(), "notes.read");
        assert_eq!(m.write_permission(), "notes.write");
        assert_eq!(m.base_path(), "/admin/notes");
        assert_eq!(m.title_column().name, "title");
    }

    #[test]
    fn list_defaults_to_all_visible_non_pk_columns() {
        static FIELDS: [UiField; 3] = [
            ui_field_const("id"),
            ui_field_const("title"),
            UiField {
                hidden: true,
                ..ui_field_const("secret")
            },
        ];
        let m = meta(
            vec![
                column("id", SqlType::Integer),
                column("title", SqlType::Text),
                column("secret", SqlType::Text),
            ],
            &FIELDS,
        );
        let names: Vec<&str> = m.list_columns().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["title"]);
    }

    #[test]
    fn form_columns_map_the_macro_computed_set() {
        static FIELDS: [UiField; 3] = [
            ui_field_const("id"),
            ui_field_const("title"),
            ui_field_const("created_at"),
        ];
        let m = meta_with(
            vec![
                column("id", SqlType::Integer),
                column("title", SqlType::Text),
                column("created_at", SqlType::Timestamp),
            ],
            &FIELDS,
            &["title"],
        );
        let names: Vec<&str> = m.form_columns().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["title"]);
    }

    const fn ui_field_const(name: &'static str) -> UiField {
        UiField {
            name,
            list: false,
            search: false,
            filter: false,
            readonly: false,
            hidden: false,
            widget: UiWidget::Text,
            options: None,
        }
    }
}
