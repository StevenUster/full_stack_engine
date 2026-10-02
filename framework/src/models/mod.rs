//! The runtime model registry — the core of struct-defined apps.
//!
//! `#[model(...)]` (from `full_stack_engine_macros`, re-exported in the
//! prelude) parses a struct with the same fse-schema code the ORM uses,
//! validates the `#[model(...)]`/`#[ui(...)]` attributes at compile time, and
//! submits a [`ModelRegistration`] (or, for `link = ...` join tables, a
//! [`LinkRegistration`]) here via `inventory`. At boot [`check`] validates
//! what spans models, then [`mount_all`] mounts every generated route
//! ([`route_table`] lists them); the theme's generic templates render from the
//! same metadata, and the app's [`ModelHooks`] carry its rules. Models defined
//! in dependency crates (modules) register through the exact same path —
//! linking the crate is enough.
//!
//! Nothing in this module validates dev input: everything expressible in the
//! attributes was already checked by the macro. This module only resolves
//! conventions (permission names, paths, default column sets) that need the
//! whole picture at runtime.

use fse_schema::{ColumnDef, TableDef};
use std::sync::LazyLock;

pub mod form;
mod hooks;
pub mod openapi;
mod resource;
mod routes;

pub use routes::{RouteInfo, RouteKind, mount_all, route_table};

/// Context injector installed by [`crate::FrameworkApp::models`]: gives
/// every page what a theme needs to draw app navigation without app code.
///
/// - `nav`: `[{ "table", "href" }]` — the admin page of every enabled model
///   the signed-in user may read (`<base>.read`), in registration order.
///   Labels are the theme's job (`t.models[table].title`).
/// - `user`: `{ "id", "role", "is_admin", "can_read_users" }` when signed in
///   (absent otherwise).
///
/// Only the token is checked here (no database round trip) — it decides
/// what links to show, never what a request may do; every generated
/// endpoint re-checks permissions itself.
pub fn inject_nav<R: crate::structs::Role>(
    req: &actix_web::HttpRequest,
    value: &mut serde_json::Value,
) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    let Ok(claims) = crate::auth::read_jwt::<R>(req) else {
        return;
    };
    let nav: Vec<serde_json::Value> = registered_models()
        .iter()
        .filter(|m| !m.ui.disabled && claims.role.has_permission(&m.read_permission()))
        .map(|m| serde_json::json!({ "table": m.table.name, "href": m.base_path() }))
        .collect();
    obj.insert("nav".to_string(), serde_json::json!(nav));
    obj.insert(
        "user".to_string(),
        serde_json::json!({
            "id": claims.sub,
            "role": claims.role.as_str(),
            "is_admin": claims.role.is_admin(),
            "can_read_users": claims.role.has_permission("users.read"),
        }),
    );
}

pub use hooks::{Access, ActionCx, CurrentUser, ModelHooks, Ref, RowView, SaveCx};
pub use resource::{
    Db, DbResult, FieldError, FormData, FormErrors, LinkResource, ListQuery, ListResult,
    ModelResource, and_opt,
};

// Re-exported under stable framework paths for the code `#[model]` emits and
// for hook implementations (scopes are `Cond`s).
pub use crate::error::{AppError, AppResult};
pub use fse_orm::Cond;

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
    /// The column shown as a row's title — `title_field = name`, else the
    /// first visible plain text column, else the primary key (resolved by
    /// the macro).
    pub title_field: &'static str,
    /// `owner = user_id` — the column holding the owning user's id. Filled
    /// from the signed-in user on create and never part of generated forms;
    /// non-admins only see their own rows.
    pub owner: Option<&'static str>,
    /// `parent = event_id` — the foreign key to the parent model this one
    /// is nested under (routes, scope, permission base).
    pub parent: Option<&'static str>,
    /// `order_by = "-date"` — the list's default order: column, descending.
    pub order_by: Option<(&'static str, bool)>,
    /// `per_page = 25` — the list's default page size.
    pub per_page: Option<i64>,
    /// `actions(publish, archive)` — row actions, each an
    /// `async fn name(&self, cx: ActionCx<'_>)` on the model.
    pub actions: &'static [&'static str],
    /// The struct's `#[orm(relation = ...)]` fields.
    pub relations: &'static [UiRelation],
    /// One entry per database column, in declaration order.
    pub fields: &'static [UiField],
    /// The columns generated create/edit forms expose, in order — computed
    /// by the macro (visible, editable, not the pk, not `default = now`,
    /// not json, not owner/parent) and the exact set the generated
    /// `create`/`update` code binds, so the two can never drift.
    pub form_fields: &'static [&'static str],
}

/// One `#[orm(relation = fk)]` field and how generated pages use it.
#[derive(Debug, Clone, Copy)]
pub struct UiRelation {
    /// The relation field (`run`).
    pub field: &'static str,
    /// The foreign-key column it joins through (`run_id`).
    pub column: &'static str,
    /// The related struct (`Run`).
    pub target: &'static str,
    /// `#[ui(show)]` / `#[ui(list)]` — embed `{ id, title }` of the related
    /// row into every generated row under the field's name.
    pub show: bool,
    /// `#[ui(list)]` — also a column of the generated list.
    pub list: bool,
    /// `#[ui(show(event))]` — relations of the related row to embed one
    /// level deeper (`row.run.event.title`).
    pub with: &'static [&'static str],
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
    /// `#[ui(filter)]` — offer a list filter; the kind defaults from the
    /// column type (`#[ui(filter = range)]` etc. to choose).
    pub filter: Option<UiFilter>,
    /// `#[ui(readonly)]` — show but never edit in generated forms.
    pub readonly: bool,
    /// `#[ui(hidden)]` — never show in generated UI or JSON. json/blob
    /// columns and secret-looking names (`password`, `*_token`, `secret`,
    /// `*_hash`, `api_key`) are hidden by default.
    pub hidden: bool,
    /// `#[ui(private)]` — shown to signed-in users (admin pages, the
    /// authenticated API) but never in `public_read` pages or the public
    /// API.
    pub private: bool,
    /// `#[ui(required)]` (or a NOT NULL column without default) — the form
    /// rejects an empty value.
    pub required: bool,
    /// `#[ui(format = date|datetime|time|number|currency)]` — rows carry a
    /// locale-formatted `{col}_display` next to the raw value.
    pub format: Option<&'static str>,
    pub widget: UiWidget,
    /// For [`UiWidget::Select`]: yields the `DbEnum`'s stored values. A fn
    /// pointer because the derive cannot see the enum's variants — only the
    /// generated `VARIANTS` const on the enum type can.
    pub options: Option<fn() -> Vec<&'static str>>,
}

/// How a `#[ui(filter)]` column filters the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiFilter {
    /// Equality, offered as a dropdown — the default for `DbEnum` and
    /// `bool` columns. Query param: the column name.
    Exact,
    /// Substring match — the default for plain text. Query param: the
    /// column name.
    Contains,
    /// Inclusive bounds — the default for numbers, dates and timestamps.
    /// Query params: `{col}_from` and `{col}_to`, either optional.
    Range,
}

impl UiFilter {
    /// The lowercase name templates switch on.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            UiFilter::Exact => "exact",
            UiFilter::Contains => "contains",
            UiFilter::Range => "range",
        }
    }

    /// The query-string parameters this filter reads for `column`.
    #[must_use]
    pub fn params(self, column: &str) -> Vec<String> {
        match self {
            UiFilter::Exact | UiFilter::Contains => vec![column.to_string()],
            UiFilter::Range => vec![format!("{column}_from"), format!("{column}_to")],
        }
    }
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
    Date,
    Select,
    /// A foreign key: a select of the related model's rows the user may read.
    Relation,
    Email,
    Url,
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
            UiWidget::Date => "date",
            UiWidget::Select => "select",
            UiWidget::Relation => "relation",
            UiWidget::Email => "email",
            UiWidget::Url => "url",
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

/// One `#[model(link = owner_col)]` join table as submitted by the macro.
pub struct LinkRegistration {
    pub table_json: &'static str,
    pub ui: &'static UiLink,
    pub resource: &'static dyn LinkResource,
}

inventory::collect!(LinkRegistration);

/// Configuration of a many-to-many join table.
#[derive(Debug, Clone, Copy)]
pub struct UiLink {
    /// `path = "managers"` — URL segment under the owner row (default: the
    /// table name).
    pub path: Option<&'static str>,
    /// The foreign key to the model that owns the links (`event_id`).
    pub owner_column: &'static str,
    /// The owning struct (`Event`).
    pub owner: &'static str,
    /// The foreign key to the linked model (`user_id`).
    pub other_column: &'static str,
    /// The linked struct (`User`).
    pub other: &'static str,
}

/// A registered join table: links rows of `ui.owner` to rows of `ui.other`.
pub struct LinkMeta {
    pub table: TableDef,
    pub ui: &'static UiLink,
    pub resource: &'static dyn LinkResource,
}

impl LinkMeta {
    /// The URL segment under the owner row (`/event-manager/{id}/{segment}`).
    #[must_use]
    pub fn segment(&self) -> &str {
        self.ui.path.unwrap_or(&self.table.name)
    }

    /// The owning model.
    ///
    /// # Panics
    ///
    /// When the owner isn't a registered model — [`check`] reports that at
    /// boot first.
    #[must_use]
    pub fn owner(&self) -> &'static ModelMeta {
        model_by_struct(self.ui.owner).expect("link owner checked at boot")
    }

    /// The linked model.
    ///
    /// # Panics
    ///
    /// As [`LinkMeta::owner`].
    #[must_use]
    pub fn other(&self) -> &'static ModelMeta {
        model_by_struct(self.ui.other).expect("link target checked at boot")
    }
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

static LINKS: LazyLock<Vec<LinkMeta>> = LazyLock::new(|| {
    let mut links: Vec<LinkMeta> = inventory::iter::<LinkRegistration>()
        .map(|reg| LinkMeta {
            table: serde_json::from_str(reg.table_json)
                .expect("table_json is written by the #[model] macro and always valid"),
            ui: reg.ui,
            resource: reg.resource,
        })
        .collect();
    links.sort_by(|a, b| a.table.name.cmp(&b.table.name));
    links
});

/// Every `#[model]` struct linked into this binary, sorted by table name.
///
/// # Panics
///
/// If two models resolve to the same table name (e.g. the same struct name in
/// an app and a module) — a conflict that cannot be seen at compile time, so
/// it fails fast here instead of behaving ambiguously.
#[must_use]
pub fn registered_models() -> &'static [ModelMeta] {
    let models = &*MODELS;
    if let Some(pair) = models
        .windows(2)
        .find(|w| w[0].table.name == w[1].table.name)
    {
        panic!(
            "two models are registered for table `{}` (structs `{}` and `{}`) — rename one \
             or set #[orm(table = \"...\")]",
            pair[0].table.name, pair[0].table.struct_name, pair[1].table.struct_name
        );
    }
    models
}

/// Every `#[model(link = ...)]` join table linked into this binary.
#[must_use]
pub fn registered_links() -> &'static [LinkMeta] {
    &LINKS
}

/// Look up a registered model by SQL table name.
#[must_use]
pub fn model(table: &str) -> Option<&'static ModelMeta> {
    registered_models().iter().find(|m| m.table.name == table)
}

/// Look up a registered model by struct name (what relations and foreign
/// keys name at compile time).
#[must_use]
pub fn model_by_struct(name: &str) -> Option<&'static ModelMeta> {
    registered_models()
        .iter()
        .find(|m| m.table.struct_name == name)
}

/// Whether `user` may point a foreign key at row `id` of `target` (a struct
/// name): the user needs `<target>.read` and the row must be inside the
/// target's scope for them. A target that isn't a `#[model]` is accepted —
/// the database's foreign-key constraint still guarantees the row exists.
///
/// The generated create/update calls this for every foreign-key form field,
/// so a crafted form can't attach a row to something the user can't see.
///
/// # Errors
///
/// Database errors from the target's scope query.
pub async fn ref_visible(db: &Db, user: &CurrentUser, target: &str, id: i64) -> AppResult<bool> {
    let Some(meta) = model_by_struct(target) else {
        return Ok(true);
    };
    if !user.has_permission(&meta.read_permission()) {
        return Ok(false);
    }
    Ok(meta
        .resource
        .get(db, Access::user(user), id)
        .await?
        .is_some())
}

/// The parent rows of a nested model `user` may see: `None` = no
/// restriction, `Some(ids)` = only children of these. A user without read
/// permission on the parent sees none. The generated scope of every nested
/// model ANDs this in, so a child row is never reachable — by its own
/// routes, a foreign-key select or a crafted form — when its parent isn't.
///
/// # Errors
///
/// Database errors from the parent's scope query.
pub async fn visible_parent_ids(
    db: &Db,
    user: &CurrentUser,
    parent: &str,
) -> AppResult<Option<Vec<i64>>> {
    let Some(meta) = model_by_struct(parent) else {
        return Ok(Some(Vec::new()));
    };
    if !user.has_permission(&meta.read_permission()) {
        return Ok(Some(Vec::new()));
    }
    meta.resource.visible_ids(db, Access::user(user)).await
}

/// Checks what no single `#[model]` expansion can see: that parents,
/// shown relations and link ends name registered models, that nesting is
/// one level deep, and that no two models claim the same URL. Every problem
/// is reported together.
///
/// [`mount_all`] panics on an error (a broken app must not boot);
/// `testing::TestApp` runs it too, so `cargo test` catches it first.
///
/// # Errors
///
/// One line per problem.
// One pass per rule, kept together so every boot problem is listed at once.
#[allow(clippy::too_many_lines)]
pub fn check() -> Result<(), String> {
    let mut problems = Vec::new();
    let models = registered_models();
    for m in models {
        let name = &m.table.struct_name;
        if let Some(column) = m.ui.parent {
            match m.parent() {
                None => problems.push(format!(
                    "{name}: parent column `{column}` must reference a #[model] struct"
                )),
                Some(parent) if parent.ui.parent.is_some() => problems.push(format!(
                    "{name}: parent `{}` is itself nested — only one level of nesting is \
                     supported",
                    parent.table.struct_name
                )),
                Some(_) => {}
            }
            if m.ui.public_read.is_some() || m.ui.api {
                problems.push(format!(
                    "{name}: a nested model can't be public_read or api — its rows only exist \
                     under a parent"
                ));
            }
        }
        // Children and links share the `{base}/{id}/{segment}` space.
        let mut segments: Vec<&str> = models
            .iter()
            .filter(|c| c.ui.parent.is_some() && c.parent().is_some_and(|p| std::ptr::eq(p, m)))
            .map(ModelMeta::segment)
            .chain(
                registered_links()
                    .iter()
                    .filter(|l| l.ui.owner == *name)
                    .map(LinkMeta::segment),
            )
            .collect();
        segments.sort_unstable();
        for pair in segments.windows(2) {
            if pair[0] == pair[1] {
                problems.push(format!(
                    "{name}: two nested models/links use the segment `{}` — set path = \"...\"",
                    pair[0]
                ));
            }
        }
        if segments
            .iter()
            .any(|s| ["actions", "create", "delete"].contains(s))
        {
            problems.push(format!(
                "{name}: `actions`, `create` and `delete` are reserved segments under a row"
            ));
        }
        for rel in m.ui.relations.iter().filter(|r| r.show) {
            let Some(target) = model_by_struct(rel.target) else {
                problems.push(format!(
                    "{name}.{}: #[ui(show)] needs `{}` to be a #[model] struct (it may be \
                     `disabled`)",
                    rel.field, rel.target
                ));
                continue;
            };
            for deeper in rel.with {
                let ok = target
                    .ui
                    .relations
                    .iter()
                    .find(|r| r.field == *deeper)
                    .is_some_and(|r| model_by_struct(r.target).is_some());
                if !ok {
                    problems.push(format!(
                        "{name}.{}: show({deeper}) — `{}` has no relation `{deeper}` to a \
                         #[model] struct",
                        rel.field, rel.target
                    ));
                }
            }
        }
    }
    for link in registered_links() {
        for end in [link.ui.owner, link.ui.other] {
            if model_by_struct(end).is_none() {
                problems.push(format!(
                    "{}: link end `{end}` must be a #[model] struct",
                    link.table.struct_name
                ));
            }
        }
    }
    // Any two generated routes on the same method + path: the first one
    // registered would silently shadow the other (e.g. an admin `path`
    // equal to the public `/{table}` pages).
    if problems.is_empty() {
        let mut routes: Vec<(String, String)> = route_table()
            .into_iter()
            .map(|r| {
                (
                    format!("{} {}", r.method, r.path),
                    format!("{} {:?}", r.model, r.kind),
                )
            })
            .collect();
        routes.sort();
        for pair in routes.windows(2) {
            if pair[0].0 == pair[1].0 {
                problems.push(format!(
                    "`{}` is generated twice ({} and {}) — give one a different \
                     #[model(path = \"...\")]",
                    pair[0].0, pair[0].1, pair[1].1
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "invalid model setup ({} problem(s)):\n  - {}",
            problems.len(),
            problems.join("\n  - ")
        ))
    }
}

impl ModelMeta {
    /// The base permission name: `permission = "..."`, else the parent's
    /// (a nested model belongs to its parent's feature), else the table
    /// name. Generated read endpoints require `<base>.read`, everything
    /// that changes data `<base>.write`.
    #[must_use]
    pub fn permission_base(&self) -> &str {
        if let Some(p) = self.ui.permission {
            return p;
        }
        if let Some(parent) = self.parent() {
            return parent.permission_base();
        }
        &self.table.name
    }

    #[must_use]
    pub fn read_permission(&self) -> String {
        format!("{}.read", self.permission_base())
    }

    #[must_use]
    pub fn write_permission(&self) -> String {
        format!("{}.write", self.permission_base())
    }

    /// The model this one is nested under (`parent = fk`).
    #[must_use]
    pub fn parent(&self) -> Option<&'static ModelMeta> {
        let column = self.table.column(self.ui.parent?)?;
        model_by_struct(&column.references.as_ref()?.table)
    }

    /// The join tables whose links this model owns.
    pub fn links(&self) -> impl Iterator<Item = &'static LinkMeta> + '_ {
        registered_links()
            .iter()
            .filter(|l| l.ui.owner == self.table.struct_name)
    }

    /// The URL segment of a nested model (`path` or the table name).
    #[must_use]
    pub fn segment(&self) -> &str {
        self.ui.path.unwrap_or(&self.table.name)
    }

    /// The base URL path of the generated admin UI, with a leading slash.
    /// An explicit `path = "product-manager"` mounts verbatim at
    /// `/product-manager`; the default is `/admin/{table}` so `public_read`
    /// pages can own the bare `/{table}` paths. A nested model has no fixed
    /// base path — see [`base_path_in`](Self::base_path_in).
    #[must_use]
    pub fn base_path(&self) -> String {
        match self.ui.path {
            Some(p) => format!("/{p}"),
            None => format!("/admin/{}", self.table.name),
        }
    }

    /// The route pattern the admin UI mounts at: the base path, or for a
    /// nested model `{parent base}/{parent_id}/{segment}`.
    #[must_use]
    pub fn base_pattern(&self) -> String {
        match self.parent() {
            Some(parent) => format!("{}/{{parent_id}}/{}", parent.base_path(), self.segment()),
            None => self.base_path(),
        }
    }

    /// The concrete base path for links and redirects: [`base_pattern`]
    /// with the parent id filled in.
    ///
    /// [`base_pattern`]: Self::base_pattern
    #[must_use]
    pub fn base_path_in(&self, parent_id: Option<i64>) -> String {
        match (self.parent(), parent_id) {
            (Some(parent), Some(id)) => {
                format!("{}/{id}/{}", parent.base_path(), self.segment())
            }
            _ => self.base_path(),
        }
    }

    /// The template namespace of the admin pages: the base path without its
    /// leading slash (`admin/posts`, `product-manager`), or for a nested model
    /// `{parent namespace}/{segment}` (`event-manager/runs`).
    #[must_use]
    pub fn template_namespace(&self) -> String {
        match self.parent() {
            Some(parent) => format!("{}/{}", parent.template_namespace(), self.segment()),
            None => self.base_path()[1..].to_string(),
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

    /// Columns offered as list filters (`#[ui(filter)]`).
    #[must_use]
    pub fn filter_columns(&self) -> Vec<&ColumnDef> {
        self.ui
            .fields
            .iter()
            .filter(|f| f.filter.is_some())
            .map(|f| self.column_of(f))
            .collect()
    }

    /// Every query-string parameter the list filters read, with its column's
    /// filter kind — `{col}` or `{col}_from`/`{col}_to`.
    #[must_use]
    pub fn filter_params(&self) -> Vec<String> {
        self.ui
            .fields
            .iter()
            .filter_map(|f| f.filter.map(|kind| kind.params(f.name)))
            .flatten()
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

    /// The column shown as a row's title (`title_field`, resolved by the
    /// macro).
    ///
    /// # Panics
    ///
    /// Never in practice: the macro resolves `title_field` from the same
    /// struct's columns.
    #[must_use]
    pub fn title_column(&self) -> &ColumnDef {
        self.table
            .column(self.ui.title_field)
            .expect("title_field resolved by the macro")
    }

    /// The relation joining through foreign-key `column`, if any.
    #[must_use]
    pub fn relation_for(&self, column: &str) -> Option<&'static UiRelation> {
        self.ui.relations.iter().find(|r| r.column == column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fse_schema::SqlType;

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
        title_field: "title",
        owner: None,
        parent: None,
        order_by: None,
        per_page: None,
        actions: &[],
        relations: &[],
        fields: &[],
        form_fields: &[],
    };

    /// A resource that must never be called — meta-resolution tests only.
    struct NoResource;

    impl ModelResource for NoResource {
        fn list<'a>(
            &'a self,
            _: &'a Db,
            _: Access<'a>,
            _: &'a ListQuery,
        ) -> BoxFuture<'a, AppResult<ListResult>> {
            unreachable!()
        }
        fn get<'a>(
            &'a self,
            _: &'a Db,
            _: Access<'a>,
            _: i64,
        ) -> BoxFuture<'a, AppResult<Option<RowView>>> {
            unreachable!()
        }
        fn get_by_public<'a>(
            &'a self,
            _: &'a Db,
            _: &'a str,
        ) -> BoxFuture<'a, AppResult<Option<serde_json::Value>>> {
            unreachable!()
        }
        fn visible_ids<'a>(
            &'a self,
            _: &'a Db,
            _: Access<'a>,
        ) -> BoxFuture<'a, AppResult<Option<Vec<i64>>>> {
            unreachable!()
        }
        fn refs<'a>(
            &'a self,
            _: &'a Db,
            _: &'a [i64],
        ) -> BoxFuture<'a, AppResult<std::collections::HashMap<i64, Ref>>> {
            unreachable!()
        }
        fn can_create<'a>(&'a self, _: &'a Db, _: Access<'a>) -> BoxFuture<'a, AppResult<bool>> {
            unreachable!()
        }
        fn create<'a>(
            &'a self,
            _: &'a Db,
            _: Access<'a>,
            _: &'a FormData,
        ) -> BoxFuture<'a, AppResult<Result<i64, FormErrors>>> {
            unreachable!()
        }
        fn update<'a>(
            &'a self,
            _: &'a Db,
            _: Access<'a>,
            _: i64,
            _: &'a FormData,
        ) -> BoxFuture<'a, AppResult<Result<(), FormErrors>>> {
            unreachable!()
        }
        fn delete<'a>(&'a self, _: &'a Db, _: Access<'a>, _: i64) -> BoxFuture<'a, AppResult<()>> {
            unreachable!()
        }
        fn act<'a>(
            &'a self,
            _: &'a Db,
            _: Access<'a>,
            _: i64,
            _: &'a str,
            _: &'a FormData,
        ) -> BoxFuture<'a, AppResult<()>> {
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
            vec![
                column("id", SqlType::Integer),
                column("title", SqlType::Text),
            ],
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
            filter: None,
            readonly: false,
            hidden: false,
            private: false,
            required: false,
            format: None,
            widget: UiWidget::Text,
            options: None,
        }
    }
}
