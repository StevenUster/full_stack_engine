//! App logic on the model: who may see and change which rows, what happens
//! around a write, and the row actions a model offers.
//!
//! Generated CRUD alone only checks role permissions (`<base>.read` /
//! `<base>.write`). [`ModelHooks`] is how an app says the rest — "a manager
//! only sees the events they manage", "a donation is locked once received",
//! "fill the slug from the name" — next to the struct, without writing the
//! handlers itself:
//!
//! ```ignore
//! #[model(path = "event-manager", permission = "events", hooks)]
//! pub struct Event { /* ... */ }
//!
//! impl ModelHooks for Event {
//!     async fn scope(db: &Db, user: &CurrentUser) -> AppResult<Option<Cond>> {
//!         if user.is_admin() {
//!             return Ok(None);
//!         }
//!         let ids = /* ids of the events this user manages */;
//!         Ok(Some(Event::ID.in_(ids)))
//!     }
//! }
//! ```
//!
//! `#[model(hooks)]` declares that the app implements the trait; without it
//! the macro emits an empty implementation, so every method below has a
//! default that changes nothing. Common cases need no trait at all:
//! `#[model(owner = user_id)]`, `#[ui(slug_from = name)]`, `#[ui(required)]`,
//! `#[ui(email)]` … (see the macro docs and `AGENTS.md`).
//!
//! Every method runs inside the generated handlers *after* the role
//! permission check, so hooks only ever narrow access — they cannot grant
//! what the role lacks.

use std::future::Future;

use fse_orm::Cond;
use serde_json::{Map, Value};

use super::resource::{Db, FormData, FormErrors};
use crate::auth::Claims;
use crate::error::AppResult;
use crate::structs::Role;

/// The signed-in user as model hooks see it.
///
/// The model registry is built before the app's role enum is known, so this
/// carries the role as its stored string plus a permission check
/// monomorphized for the app's enum when the routes were mounted.
/// [`CurrentUser::role`] turns it back into that enum:
///
/// ```ignore
/// if matches!(user.role::<AppRole>(), AppRole::Admin | AppRole::RunnerAdmin) { ... }
/// ```
#[derive(Clone)]
pub struct CurrentUser {
    id: i64,
    role: String,
    is_admin: bool,
    check: fn(&str, &str) -> bool,
}

fn check_permission<R: Role>(role: &str, permission: &str) -> bool {
    R::from_role_str(role).has_permission(permission)
}

impl CurrentUser {
    /// A user with the given id and role — what the generated handlers build
    /// from the request's token, and what tests use to call a model's
    /// [`ModelResource`](super::ModelResource) directly.
    #[must_use]
    pub fn new<R: Role>(id: i64, role: &R) -> Self {
        Self {
            id,
            role: role.as_str().to_string(),
            is_admin: role.is_admin(),
            check: check_permission::<R>,
        }
    }

    #[must_use]
    pub fn from_claims<R: Role>(claims: &Claims<R>) -> Self {
        Self::new(claims.sub, &claims.role)
    }

    /// The user's primary key.
    #[must_use]
    pub fn id(&self) -> i64 {
        self.id
    }

    /// The app's own role enum. `R` must be the role type the routes were
    /// mounted with (`FrameworkApp::models::<R>()`); any other type parses
    /// the stored role name through its own `from_role_str`.
    #[must_use]
    pub fn role<R: Role>(&self) -> R {
        R::from_role_str(&self.role)
    }

    /// The role's stored name (`Role::as_str`).
    #[must_use]
    pub fn role_str(&self) -> &str {
        &self.role
    }

    /// `Role::is_admin` of the user's role.
    #[must_use]
    pub fn is_admin(&self) -> bool {
        self.is_admin
    }

    /// `Role::has_permission` of the user's role.
    #[must_use]
    pub fn has_permission(&self, permission: &str) -> bool {
        (self.check)(&self.role, permission)
    }
}

impl std::fmt::Debug for CurrentUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CurrentUser")
            .field("id", &self.id)
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

/// Who is asking, and from where: the signed-in user (`None` for an
/// anonymous visitor of the `public_read` pages/API) and, for a nested
/// model (`#[model(parent = ...)]`), the parent row the request is under.
///
/// Every generated read and write takes one, so the model's access rules —
/// `owner`, [`ModelHooks::scope`] / [`public_scope`](ModelHooks::public_scope)
/// and the parent filter — are applied in exactly one place.
#[derive(Clone, Copy, Debug)]
pub struct Access<'a> {
    pub user: Option<&'a CurrentUser>,
    /// The parent row's id for a nested model; rows of other parents are
    /// out of scope, and creates are filed under it.
    pub parent_id: Option<i64>,
}

impl<'a> Access<'a> {
    /// An anonymous visitor: `public_scope` applies.
    pub const PUBLIC: Access<'static> = Access {
        user: None,
        parent_id: None,
    };

    /// A signed-in user: `owner` and `scope` apply.
    #[must_use]
    pub fn user(user: &'a CurrentUser) -> Self {
        Self {
            user: Some(user),
            parent_id: None,
        }
    }

    /// The same access, restricted to the children of one parent row.
    #[must_use]
    pub fn in_parent(self, parent_id: i64) -> Self {
        Self {
            parent_id: Some(parent_id),
            ..self
        }
    }
}

/// Context handed to [`ModelHooks::before_save`].
pub struct SaveCx<'a, M> {
    pub db: &'a Db,
    pub user: &'a CurrentUser,
    /// The row as stored before this write; `None` when creating.
    pub existing: Option<&'a M>,
    /// The parent row's id when a nested model is created/updated under it.
    pub parent_id: Option<i64>,
}

/// Context handed to a row action — `#[model(actions(name))]` calls
/// `async fn name(&self, cx: ActionCx<'_>) -> AppResult<()>` on the model.
pub struct ActionCx<'a> {
    pub db: &'a Db,
    pub user: &'a CurrentUser,
    /// Whatever the action's form posted (empty for a plain button).
    pub form: &'a FormData,
}

/// Per-model logic the generated CRUD calls. Implement it in the model's
/// own file and mark the struct `#[model(hooks)]`; every method is optional.
///
/// Async methods can be written as plain `async fn` in the impl.
pub trait ModelHooks: Sized + Send + Sync + 'static {
    /// The rows `user` may see at all, as a condition joined with `AND` into
    /// the `WHERE` of the generated list, detail, update, delete and
    /// actions — one rule for all of them. `None` (the default) means every
    /// row. A row outside the scope is a 404, so its existence doesn't leak.
    ///
    /// `#[model(owner = col)]` and a nested model's parent filter add their
    /// own conditions on top of this one.
    #[must_use]
    fn scope(db: &Db, user: &CurrentUser) -> impl Future<Output = AppResult<Option<Cond>>> + Send {
        let _ = (db, user);
        async { Ok(None) }
    }

    /// The rows the public (`public_read`) pages and the public JSON API may
    /// show — e.g. only published ones. `None` (the default) means every row.
    #[must_use]
    fn public_scope() -> Option<Cond> {
        None
    }

    /// Whether `user` may create rows (on top of `<base>.write`) — for a
    /// nested model, under the parent row `parent_id` (e.g. "not once the
    /// event is completed"). Also decides whether the create button shows.
    #[must_use]
    fn can_create(
        db: &Db,
        user: &CurrentUser,
        parent_id: Option<i64>,
    ) -> impl Future<Output = AppResult<bool>> + Send {
        let _ = (db, user, parent_id);
        async { Ok(true) }
    }

    /// Whether `user` may change this row, e.g. "not once the event is
    /// completed". Only asked for rows inside [`scope`](Self::scope). A
    /// refusal is `AppError::NoAuth`, and the edit page renders read-only.
    fn can_edit(
        &self,
        db: &Db,
        user: &CurrentUser,
    ) -> impl Future<Output = AppResult<bool>> + Send {
        let _ = (db, user);
        async { Ok(true) }
    }

    /// Whether `user` may delete this row. Defaults to
    /// [`can_edit`](Self::can_edit): a row too locked to change is too locked
    /// to delete.
    fn can_delete(
        &self,
        db: &Db,
        user: &CurrentUser,
    ) -> impl Future<Output = AppResult<bool>> + Send {
        self.can_edit(db, user)
    }

    /// Whether `user` may run the row action `action` on this row (on top of
    /// `<base>.write`). Decides both the button and the request.
    fn can_act(
        &self,
        action: &str,
        db: &Db,
        user: &CurrentUser,
    ) -> impl Future<Output = AppResult<bool>> + Send {
        let _ = (action, db, user);
        async { Ok(true) }
    }

    /// Runs on every generated create/update before the submitted form is
    /// parsed: normalize or fill fields (`form.insert("slug", ...)`) and
    /// return field errors for input the column types can't judge. A
    /// non-empty result re-renders the form, together with any parse errors.
    ///
    /// It works on the raw submission because the generated write binds
    /// parsed form values straight into the checked `insert!`/`update!` —
    /// there is no half-built struct to hand over.
    fn before_save(
        form: &mut FormData,
        cx: SaveCx<'_, Self>,
    ) -> impl Future<Output = AppResult<FormErrors>> + Send {
        let _ = (form, cx);
        async { Ok(Vec::new()) }
    }

    /// Runs after a generated create (`created`) or update was written, with
    /// the row as now stored. The write is already committed: an error here
    /// becomes the response but does not undo it.
    fn after_save(
        &self,
        db: &Db,
        user: &CurrentUser,
        created: bool,
    ) -> impl Future<Output = AppResult<()>> + Send {
        let _ = (db, user, created);
        async { Ok(()) }
    }

    /// Runs right before a generated delete, after
    /// [`can_delete`](Self::can_delete) passed — for cleanup that needs rows
    /// the delete is about to cascade away (e.g. files listed in a child
    /// table).
    fn before_delete(
        &self,
        db: &Db,
        user: &CurrentUser,
    ) -> impl Future<Output = AppResult<()>> + Send {
        let _ = (db, user);
        async { Ok(()) }
    }

    /// Runs after a generated delete, with the row as it was — e.g. to remove
    /// files the row pointed at.
    fn after_delete(
        self,
        db: &Db,
        user: &CurrentUser,
    ) -> impl Future<Output = AppResult<()>> + Send {
        let _ = (db, user);
        async { Ok(()) }
    }

    /// Add computed display fields to the row JSON every generated page and
    /// API response carries (`row.insert("label".into(), ...)`). For plain
    /// formatting prefer `#[ui(format = date|currency|...)]`.
    fn decorate(&self, row: &mut Map<String, Value>) {
        let _ = row;
    }
}

/// One row as a detail/edit page needs it.
#[derive(Debug, Clone)]
pub struct RowView {
    pub row: Value,
    /// [`ModelHooks::can_edit`] for the requesting user (`false` for
    /// anonymous access).
    pub can_edit: bool,
    pub can_delete: bool,
    /// The row actions [`ModelHooks::can_act`] allows on this row.
    pub actions: Vec<&'static str>,
}

/// What a related row contributes to a row that points at it: its title,
/// and the foreign keys of its own relations (for embedding one level
/// deeper). Built by [`ModelResource::refs`](super::ModelResource::refs).
#[derive(Debug, Clone)]
pub struct Ref {
    pub title: Value,
    /// `(relation field, foreign key value)` per relation of the related row.
    pub links: Vec<(&'static str, Option<i64>)>,
}
