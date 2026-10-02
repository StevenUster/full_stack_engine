//! Generic CRUD HTTP handlers, mounted at boot for every `#[model]` struct
//! via [`crate::FrameworkApp::models`].
//!
//! Every route a model gets is listed once, by [`model_routes`] /
//! [`link_routes`]; [`mount_all`] mounts that list and [`route_table`]
//! prints it (`cargo run -- --routes`), so the two cannot drift.
//!
//! Overriding: app routes are registered *before* these (actix matches in
//! registration order), so a same-path route in the app's `configure` simply
//! shadows the generated one. Templates are chosen per model — a template
//! named after the model's namespace (e.g. `admin/posts`, `admin/posts/form`,
//! `event-manager/runs`, `posts`, `posts/detail`) wins over the theme's
//! generic `fse/*` templates, so one page can be overridden with zero Rust.
//!
//! Every handler enforces the model's conventional permissions through
//! [`AuthUser`] — `<base>.read` for pages that show data, `<base>.write`
//! for anything that changes it — and every data access goes through the
//! model's [`Access`] (owner, scope hooks, parent). `public_read` pages and
//! their JSON API carry no auth by design and use `public_scope`.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use actix_web::http::header::LOCATION;
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};

use super::{
    Access, CurrentUser, FieldError, FormData, LinkMeta, ListQuery, ListResult, ModelMeta,
    model_by_struct, registered_links, registered_models,
};
use crate::auth::AuthUser;
use crate::error::{AppError, AppResult};
use crate::structs::Role;
use crate::{AppData, RenderTplExt};

const DEFAULT_PER_PAGE: i64 = 20;
const MAX_PER_PAGE: i64 = 100;
/// Rows offered in a foreign-key select. A larger target needs a custom
/// page with a search field.
const MAX_OPTIONS: i64 = 200;
/// Rows a link page's candidate search returns.
const MAX_CANDIDATES: i64 = 50;

// ------------------------------------------------------------ route specs

/// What a generated route does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    List,
    CreateForm,
    Create,
    Edit,
    Update,
    Delete,
    /// `POST {base}/{id}/delete` — the same delete for plain HTML forms
    /// (no JavaScript needed), answered with a redirect.
    DeleteForm,
    Action,
    PublicList,
    PublicDetail,
    ApiList,
    ApiDetail,
    LinkPage,
    LinkCandidates,
    LinkAdd,
    LinkRemove,
}

/// One generated route, as [`route_table`] reports it.
#[derive(Debug, Clone)]
pub struct RouteInfo {
    pub method: &'static str,
    pub path: String,
    pub kind: RouteKind,
    /// The struct it belongs to.
    pub model: String,
    /// The permission a request needs (`None` = public).
    pub permission: Option<String>,
}

impl fmt::Display for RouteInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:<6} {:<48} {:<14} {:<16} {}",
            self.method,
            self.path,
            format!("{:?}", self.kind),
            self.model,
            self.permission.as_deref().unwrap_or("public")
        )
    }
}

/// The routes of one model, in mount order (`/create` before `/{id}`).
#[must_use]
pub fn model_routes(meta: &ModelMeta) -> Vec<RouteInfo> {
    let mut out = Vec::new();
    if meta.ui.disabled {
        return out;
    }
    let base = meta.base_pattern();
    let read = Some(meta.read_permission());
    let write = Some(meta.write_permission());
    let model = meta.table.struct_name.clone();
    let mut push = |method, path: String, kind, permission: &Option<String>| {
        out.push(RouteInfo {
            method,
            path,
            kind,
            model: model.clone(),
            permission: permission.clone(),
        });
    };

    push("GET", base.clone(), RouteKind::List, &read);
    if !meta.ui.no_create {
        push(
            "GET",
            format!("{base}/create"),
            RouteKind::CreateForm,
            &write,
        );
        push("POST", format!("{base}/create"), RouteKind::Create, &write);
    }
    push("GET", format!("{base}/{{id}}"), RouteKind::Edit, &read);
    if !meta.ui.no_edit {
        push("POST", format!("{base}/{{id}}"), RouteKind::Update, &write);
    }
    if !meta.ui.no_delete {
        push(
            "DELETE",
            format!("{base}/{{id}}"),
            RouteKind::Delete,
            &write,
        );
        push(
            "POST",
            format!("{base}/{{id}}/delete"),
            RouteKind::DeleteForm,
            &write,
        );
    }
    if !meta.ui.actions.is_empty() {
        push(
            "POST",
            format!("{base}/{{id}}/actions/{{action}}"),
            RouteKind::Action,
            &write,
        );
    }
    if meta.parent().is_none() {
        if meta.ui.public_read.is_some() {
            let table = &meta.table.name;
            push("GET", format!("/{table}"), RouteKind::PublicList, &None);
            push(
                "GET",
                format!("/{table}/{{key}}"),
                RouteKind::PublicDetail,
                &None,
            );
        }
        if meta.ui.api {
            let table = &meta.table.name;
            let perm = if meta.ui.public_read.is_some() {
                None
            } else {
                read.clone()
            };
            push("GET", format!("/api/{table}"), RouteKind::ApiList, &perm);
            let detail = if meta.ui.public_read.is_some() {
                "{key}"
            } else {
                "{id}"
            };
            push(
                "GET",
                format!("/api/{table}/{detail}"),
                RouteKind::ApiDetail,
                &perm,
            );
        }
    }
    out
}

/// The routes of one link table, mounted under its owner's rows.
#[must_use]
pub fn link_routes(link: &LinkMeta) -> Vec<RouteInfo> {
    let owner = link.owner();
    if owner.ui.disabled {
        return Vec::new();
    }
    let base = format!("{}/{{id}}/{}", owner.base_pattern(), link.segment());
    let read = Some(owner.read_permission());
    let write = Some(owner.write_permission());
    let model = link.table.struct_name.clone();
    [
        ("GET", base.clone(), RouteKind::LinkPage, read),
        (
            "GET",
            format!("{base}/candidates"),
            RouteKind::LinkCandidates,
            write.clone(),
        ),
        ("POST", base.clone(), RouteKind::LinkAdd, write.clone()),
        (
            "DELETE",
            format!("{base}/{{other_id}}"),
            RouteKind::LinkRemove,
            write,
        ),
    ]
    .into_iter()
    .map(|(method, path, kind, permission)| RouteInfo {
        method,
        path,
        kind,
        model: model.clone(),
        permission,
    })
    .collect()
}

/// Every generated route of the app — what `cargo run -- --routes` prints.
#[must_use]
pub fn route_table() -> Vec<RouteInfo> {
    let mut out: Vec<RouteInfo> = registered_models().iter().flat_map(model_routes).collect();
    out.extend(registered_links().iter().flat_map(link_routes));
    out
}

// ---------------------------------------------------------------- mounting

/// Mounts the generated routes for every registered, non-`disabled` model
/// and every link table. `R` is the app's role enum — permission checks run
/// against it. Called by [`crate::FrameworkApp::models`]; public so tests
/// (and unusual setups) can apply it to a raw `ServiceConfig`.
///
/// # Panics
///
/// When the model setup is inconsistent (see [`super::check`]) — a
/// boot-time check by design.
pub fn mount_all<R: Role>(cfg: &mut web::ServiceConfig) {
    if let Err(problems) = super::check() {
        panic!("{problems}");
    }
    for meta in registered_models() {
        for route in model_routes(meta) {
            mount_model_route::<R>(cfg, meta, &route);
        }
    }
    for link in registered_links() {
        for route in link_routes(link) {
            mount_link_route::<R>(cfg, link, &route);
        }
    }
}

type Query = web::Query<HashMap<String, String>>;

fn method(route: &RouteInfo) -> actix_web::Route {
    match route.method {
        "POST" => web::post(),
        "DELETE" => web::delete(),
        _ => web::get(),
    }
}

fn mount_model_route<R: Role>(
    cfg: &mut web::ServiceConfig,
    meta: &'static ModelMeta,
    route: &RouteInfo,
) {
    let r = method(route);
    let r = match route.kind {
        RouteKind::List => r.to(
            move |d: web::Data<AppData>, req: HttpRequest, u: AuthUser<R>, q: Query| {
                list_page(meta, d, req, u, q)
            },
        ),
        RouteKind::CreateForm => r.to(
            move |d: web::Data<AppData>, req: HttpRequest, u: AuthUser<R>| {
                create_form(meta, d, req, u)
            },
        ),
        RouteKind::Create => r.to(
            move |d: web::Data<AppData>,
                  req: HttpRequest,
                  u: AuthUser<R>,
                  f: web::Form<FormData>| { create_submit(meta, d, req, u, f) },
        ),
        RouteKind::Edit => r.to(
            move |d: web::Data<AppData>, req: HttpRequest, u: AuthUser<R>| {
                edit_form(meta, d, req, u)
            },
        ),
        RouteKind::Update => r.to(
            move |d: web::Data<AppData>,
                  req: HttpRequest,
                  u: AuthUser<R>,
                  f: web::Form<FormData>| { update_submit(meta, d, req, u, f) },
        ),
        RouteKind::Delete => r.to(
            move |d: web::Data<AppData>, req: HttpRequest, u: AuthUser<R>| {
                delete_row(meta, d, req, u)
            },
        ),
        // A plain button posts nothing: the form is optional.
        RouteKind::DeleteForm => r.to(
            move |d: web::Data<AppData>,
                  req: HttpRequest,
                  u: AuthUser<R>,
                  f: Option<web::Form<FormData>>| {
                delete_form(
                    meta,
                    d,
                    req,
                    u,
                    f.map(web::Form::into_inner).unwrap_or_default(),
                )
            },
        ),
        // A plain action button posts nothing: the form is optional.
        RouteKind::Action => r.to(
            move |d: web::Data<AppData>,
                  req: HttpRequest,
                  u: AuthUser<R>,
                  f: Option<web::Form<FormData>>| {
                run_action(
                    meta,
                    d,
                    req,
                    u,
                    f.map(web::Form::into_inner).unwrap_or_default(),
                )
            },
        ),
        RouteKind::PublicList => r.to(move |d: web::Data<AppData>, req: HttpRequest, q: Query| {
            public_list(meta, d, req, q)
        }),
        RouteKind::PublicDetail => {
            r.to(move |d: web::Data<AppData>, req: HttpRequest| public_detail(meta, d, req))
        }
        RouteKind::ApiList if route.permission.is_none() => {
            r.to(move |d: web::Data<AppData>, req: HttpRequest, q: Query| {
                api_list(meta, d, req, q, None)
            })
        }
        RouteKind::ApiList => r.to(
            move |d: web::Data<AppData>, req: HttpRequest, u: AuthUser<R>, q: Query| async move {
                u.require_permission(&meta.read_permission())?;
                let current = CurrentUser::from_claims(&u.claims);
                api_list(meta, d, req, q, Some(&current)).await
            },
        ),
        RouteKind::ApiDetail if route.permission.is_none() => {
            r.to(move |d: web::Data<AppData>, req: HttpRequest| api_detail_public(meta, d, req))
        }
        RouteKind::ApiDetail => r.to(
            move |d: web::Data<AppData>, req: HttpRequest, u: AuthUser<R>| {
                api_detail(meta, d, req, u)
            },
        ),
        RouteKind::LinkPage
        | RouteKind::LinkCandidates
        | RouteKind::LinkAdd
        | RouteKind::LinkRemove => {
            unreachable!("link routes are mounted by mount_link_route")
        }
    };
    cfg.route(&route.path, r);
}

fn mount_link_route<R: Role>(
    cfg: &mut web::ServiceConfig,
    link: &'static LinkMeta,
    route: &RouteInfo,
) {
    let r = method(route);
    let r = match route.kind {
        RouteKind::LinkPage => r.to(
            move |d: web::Data<AppData>, req: HttpRequest, u: AuthUser<R>| {
                link_page(link, d, req, u)
            },
        ),
        RouteKind::LinkCandidates => r.to(
            move |d: web::Data<AppData>, req: HttpRequest, u: AuthUser<R>, q: Query| {
                link_candidates(link, d, req, u, q)
            },
        ),
        RouteKind::LinkAdd => r.to(
            move |d: web::Data<AppData>,
                  req: HttpRequest,
                  u: AuthUser<R>,
                  f: web::Form<FormData>| { link_add(link, d, req, u, f) },
        ),
        RouteKind::LinkRemove => r.to(
            move |d: web::Data<AppData>, req: HttpRequest, u: AuthUser<R>| {
                link_remove(link, d, req, u)
            },
        ),
        _ => unreachable!("model routes are mounted by mount_model_route"),
    };
    cfg.route(&route.path, r);
}

// ------------------------------------------------------------ request ctx

/// An integer path parameter; a non-number is a 404 (no such row).
fn path_id(req: &HttpRequest, name: &str) -> AppResult<Option<i64>> {
    match req.match_info().get(name) {
        None => Ok(None),
        Some(raw) => raw
            .parse()
            .map(Some)
            .map_err(|_| AppError::NotFound("no such row".into())),
    }
}

fn required_id(req: &HttpRequest, name: &str) -> AppResult<i64> {
    path_id(req, name)?.ok_or_else(|| AppError::NotFound("no such row".into()))
}

/// Everything a handler of a (possibly nested) model works from, after the
/// permission check: the user, the parent row (visible to them, or 404) and
/// the concrete base path.
struct Cx {
    user: CurrentUser,
    parent_id: Option<i64>,
    /// `{ meta: {...}, row: {...} }` of the parent row, for templates.
    parent: Value,
    base_path: String,
    can_write: bool,
}

impl Cx {
    fn access(&self) -> Access<'_> {
        Access {
            user: Some(&self.user),
            parent_id: self.parent_id,
        }
    }
}

async fn enter<R: Role>(
    meta: &'static ModelMeta,
    data: &AppData,
    req: &HttpRequest,
    user: &AuthUser<R>,
    write: bool,
) -> AppResult<Cx> {
    user.require_permission(&meta.read_permission())?;
    if write {
        user.require_permission(&meta.write_permission())?;
    }
    let current = CurrentUser::from_claims(&user.claims);
    let parent_id = path_id(req, "parent_id")?;
    let mut parent = Value::Null;
    if let (Some(parent_meta), Some(pid)) = (meta.parent(), parent_id) {
        user.require_permission(&parent_meta.read_permission())?;
        let view = parent_meta
            .resource
            .get(&data.db, Access::user(&current), pid)
            .await?
            .ok_or_else(|| not_found(parent_meta))?;
        let mut row = view.row;
        prepare_rows(parent_meta, data, req, std::slice::from_mut(&mut row)).await?;
        parent = json!({
            "meta": {
                "table": parent_meta.table.name,
                "base_path": parent_meta.base_path(),
                "title_field": parent_meta.ui.title_field,
            },
            "row": row,
            "can_edit": view.can_edit,
        });
    }
    Ok(Cx {
        can_write: user.claims.role.has_permission(&meta.write_permission()),
        base_path: meta.base_path_in(parent_id),
        user: current,
        parent_id,
        parent,
    })
}

// ---------------------------------------------------------------- handlers

async fn list_page<R: Role>(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
    query: Query,
) -> AppResult {
    let cx = enter(meta, &data, &req, &user, false).await?;
    let q = list_query(meta, &query);
    let mut result = meta.resource.list(&data.db, cx.access(), &q).await?;
    prepare_rows(meta, &data, &req, &mut result.rows).await?;
    let can_create = cx.can_write
        && !meta.ui.no_create
        && meta.resource.can_create(&data.db, cx.access()).await?;
    let mut ctx = list_context(meta, &cx.base_path, &q, &result, cx.can_write);
    ctx["can_create"] = json!(can_create);
    ctx["parent"] = cx.parent.clone();
    let name = template_name(&data, &meta.template_namespace(), "fse/list");
    Ok(req.render_tpl(&name, &ctx).await)
}

async fn create_form<R: Role>(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
) -> AppResult {
    let cx = enter(meta, &data, &req, &user, true).await?;
    if !meta.resource.can_create(&data.db, cx.access()).await? {
        return Err(AppError::NoAuth);
    }
    render_form(
        &data,
        &req,
        meta,
        &cx,
        &default_row(meta),
        &[],
        FormMode::Create,
    )
    .await
}

async fn create_submit<R: Role>(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
    form: web::Form<FormData>,
) -> AppResult {
    let cx = enter(meta, &data, &req, &user, true).await?;
    match meta.resource.create(&data.db, cx.access(), &form).await? {
        Ok(id) => {
            let target = back_or(&form, &cx.base_path, format!("{}/{id}", cx.base_path));
            Ok(redirect(&format!("{}{target}", data.lang_prefix(&req))))
        }
        Err(errors) => {
            let row = form_values(meta, &form);
            render_form(&data, &req, meta, &cx, &row, &errors, FormMode::Create).await
        }
    }
}

async fn edit_form<R: Role>(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
) -> AppResult {
    let cx = enter(meta, &data, &req, &user, false).await?;
    let id = required_id(&req, "id")?;
    let view = meta
        .resource
        .get(&data.db, cx.access(), id)
        .await?
        .ok_or_else(|| not_found(meta))?;
    let mut row = view.row;
    prepare_rows(meta, &data, &req, std::slice::from_mut(&mut row)).await?;
    let mode = FormMode::Edit {
        can_edit: cx.can_write && !meta.ui.no_edit && view.can_edit,
        can_delete: cx.can_write && !meta.ui.no_delete && view.can_delete,
        actions: if cx.can_write {
            view.actions
        } else {
            Vec::new()
        },
    };
    render_form(&data, &req, meta, &cx, &row, &[], mode).await
}

async fn update_submit<R: Role>(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
    form: web::Form<FormData>,
) -> AppResult {
    let cx = enter(meta, &data, &req, &user, true).await?;
    let id = required_id(&req, "id")?;
    match meta
        .resource
        .update(&data.db, cx.access(), id, &form)
        .await?
    {
        Ok(()) => {
            let target = back_or(&form, &cx.base_path, format!("{}/{id}", cx.base_path));
            Ok(redirect(&format!("{}{target}", data.lang_prefix(&req))))
        }
        Err(errors) => {
            let mut row = form_values(meta, &form);
            row["id"] = json!(id);
            // The update got past can_edit, so the form stays editable;
            // delete and actions are re-asked of the row as stored.
            let view = meta.resource.get(&data.db, cx.access(), id).await?;
            let mode = FormMode::Edit {
                can_edit: true,
                can_delete: !meta.ui.no_delete && view.as_ref().is_some_and(|v| v.can_delete),
                actions: view.map(|v| v.actions).unwrap_or_default(),
            };
            render_form(&data, &req, meta, &cx, &row, &errors, mode).await
        }
    }
}

async fn delete_row<R: Role>(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
) -> AppResult {
    let cx = enter(meta, &data, &req, &user, true).await?;
    let id = required_id(&req, "id")?;
    meta.resource.delete(&data.db, cx.access(), id).await?;
    Ok(HttpResponse::Ok().json(json!({ "ok": true })))
}

/// `POST {base}/{id}/delete`: delete, then go to `_back` (inside this
/// model's pages) or the list.
async fn delete_form<R: Role>(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
    form: FormData,
) -> AppResult {
    let cx = enter(meta, &data, &req, &user, true).await?;
    let id = required_id(&req, "id")?;
    meta.resource.delete(&data.db, cx.access(), id).await?;
    let target = back_or(&form, &cx.base_path, cx.base_path.clone());
    Ok(see_other(&format!("{}{target}", data.lang_prefix(&req))))
}

/// `POST {base}/{id}/actions/{action}`: run the row action, then go back to
/// the row — or to the list when the action removed it from view.
async fn run_action<R: Role>(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
    form: FormData,
) -> AppResult {
    let cx = enter(meta, &data, &req, &user, true).await?;
    let id = required_id(&req, "id")?;
    let action = req.match_info().get("action").unwrap_or_default();
    meta.resource
        .act(&data.db, cx.access(), id, action, &form)
        .await?;
    let still_visible = meta
        .resource
        .get(&data.db, cx.access(), id)
        .await?
        .is_some();
    let default = if still_visible {
        format!("{}/{id}", cx.base_path)
    } else {
        cx.base_path.clone()
    };
    let target = back_or(&form, &cx.base_path, default);
    Ok(see_other(&format!("{}{target}", data.lang_prefix(&req))))
}

async fn public_list(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    query: Query,
) -> AppResult {
    let q = list_query(meta, &query);
    let mut result = meta.resource.list(&data.db, Access::PUBLIC, &q).await?;
    prepare_rows(meta, &data, &req, &mut result.rows).await?;
    let mut ctx = list_context(meta, &meta.base_path(), &q, &result, false);
    // Anonymous pages get the public slice of the metadata only — never
    // the admin path, form fields or actions.
    ctx["meta"] = public_meta_context(meta);
    let name = template_name(&data, &meta.table.name, "fse/public-list");
    Ok(req.render_tpl(&name, &ctx).await)
}

async fn public_detail(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
) -> AppResult {
    let key = req.match_info().get("key").unwrap_or_default().to_string();
    let mut row = meta
        .resource
        .get_by_public(&data.db, &key)
        .await?
        .ok_or_else(|| not_found(meta))?;
    prepare_rows(meta, &data, &req, std::slice::from_mut(&mut row)).await?;
    let ctx = json!({ "meta": public_meta_context(meta), "row": row });
    let name = template_name(
        &data,
        &format!("{}/detail", meta.table.name),
        "fse/public-detail",
    );
    Ok(req.render_tpl(&name, &ctx).await)
}

async fn api_list(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    query: Query,
    user: Option<&CurrentUser>,
) -> AppResult {
    let q = list_query(meta, &query);
    let access = Access {
        user,
        parent_id: None,
    };
    let mut result = meta.resource.list(&data.db, access, &q).await?;
    prepare_rows(meta, &data, &req, &mut result.rows).await?;
    Ok(api_list_response(&result))
}

async fn api_detail_public(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
) -> AppResult {
    let key = req.match_info().get("key").unwrap_or_default().to_string();
    let mut row = meta
        .resource
        .get_by_public(&data.db, &key)
        .await?
        .ok_or_else(|| not_found(meta))?;
    prepare_rows(meta, &data, &req, std::slice::from_mut(&mut row)).await?;
    Ok(HttpResponse::Ok().json(row))
}

async fn api_detail<R: Role>(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
) -> AppResult {
    user.require_permission(&meta.read_permission())?;
    let current = CurrentUser::from_claims(&user.claims);
    let id = required_id(&req, "id")?;
    let mut row = meta
        .resource
        .get(&data.db, Access::user(&current), id)
        .await?
        .ok_or_else(|| not_found(meta))?
        .row;
    prepare_rows(meta, &data, &req, std::slice::from_mut(&mut row)).await?;
    Ok(HttpResponse::Ok().json(row))
}

// ------------------------------------------------------------------- links

/// The owner row of a link request, checked: owner permission (`write` for
/// changes), read permission on the linked model (you can only see and link
/// what you may read), and the owner row inside the user's scope.
async fn enter_link<R: Role>(
    link: &'static LinkMeta,
    data: &AppData,
    req: &HttpRequest,
    user: &AuthUser<R>,
    write: bool,
) -> AppResult<(Cx, i64, Value)> {
    let owner = link.owner();
    let cx = enter(owner, data, req, user, write).await?;
    user.require_permission(&link.other().read_permission())?;
    let owner_id = required_id(req, "id")?;
    let mut row = owner
        .resource
        .get(&data.db, cx.access(), owner_id)
        .await?
        .ok_or_else(|| not_found(owner))?
        .row;
    prepare_rows(owner, data, req, std::slice::from_mut(&mut row)).await?;
    Ok((cx, owner_id, row))
}

async fn link_page<R: Role>(
    link: &'static LinkMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
) -> AppResult {
    let (cx, owner_id, owner_row) = enter_link(link, &data, &req, &user, false).await?;
    let owner = link.owner();
    let other = link.other();
    let ids = link.resource.linked(&data.db, owner_id).await?;
    let refs = other.resource.refs(&data.db, &ids).await?;
    let mut linked: Vec<Value> = ids
        .iter()
        .filter_map(|id| refs.get(id).map(|r| json!({ "id": id, "title": r.title })))
        .collect();
    linked.sort_by_key(|v| v["title"].to_string().to_lowercase());
    let base = format!("{}/{owner_id}/{}", cx.base_path, link.segment());
    let ctx = json!({
        "meta": meta_context(owner, &cx.base_path, cx.can_write),
        "parent": {
            "meta": {
                "table": owner.table.name,
                "base_path": cx.base_path,
                "title_field": owner.ui.title_field,
            },
            "row": owner_row,
        },
        "link": {
            "table": link.table.name,
            "segment": link.segment(),
            "other_table": other.table.name,
            "base_path": base,
        },
        "linked": linked,
        "can_write": cx.can_write,
    });
    let name = template_name(
        &data,
        &format!("{}/{}", owner.template_namespace(), link.segment()),
        "fse/links",
    );
    Ok(req.render_tpl(&name, &ctx).await)
}

/// JSON `{ rows: [{ id, title, linked }] }` — the linked model's rows the
/// user may read, matching `search`, for the "add" picker.
async fn link_candidates<R: Role>(
    link: &'static LinkMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
    query: Query,
) -> AppResult {
    let (cx, owner_id, _) = enter_link(link, &data, &req, &user, true).await?;
    let other = link.other();
    let linked: std::collections::HashSet<i64> = link
        .resource
        .linked(&data.db, owner_id)
        .await?
        .into_iter()
        .collect();
    let q = ListQuery {
        search: query
            .get("search")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        page: 1,
        per_page: MAX_CANDIDATES,
        ..ListQuery::default()
    };
    let result = other
        .resource
        .list(&data.db, Access::user(&cx.user), &q)
        .await?;
    let rows: Vec<Value> = result
        .rows
        .iter()
        .map(|row| {
            let id = row["id"].as_i64().unwrap_or_default();
            json!({
                "id": id,
                "title": row[other.ui.title_field],
                "linked": linked.contains(&id),
            })
        })
        .collect();
    Ok(HttpResponse::Ok().json(json!({ "rows": rows })))
}

async fn link_add<R: Role>(
    link: &'static LinkMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
    form: web::Form<FormData>,
) -> AppResult {
    let (cx, owner_id, _) = enter_link(link, &data, &req, &user, true).await?;
    let other_id: i64 = form
        .get("id")
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| AppError::BadRequest("id is required".into()))?;
    // Only rows the user can see can be linked — same rule as a foreign-key
    // form field.
    if !super::ref_visible(&data.db, &cx.user, link.ui.other, other_id).await? {
        return Err(not_found(link.other()));
    }
    link.resource
        .add(&data.db, &cx.user, owner_id, other_id)
        .await?;
    Ok(see_other(&format!(
        "{}{}/{owner_id}/{}",
        data.lang_prefix(&req),
        cx.base_path,
        link.segment()
    )))
}

async fn link_remove<R: Role>(
    link: &'static LinkMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
) -> AppResult {
    let (cx, owner_id, _) = enter_link(link, &data, &req, &user, true).await?;
    let other_id = required_id(&req, "other_id")?;
    if link
        .resource
        .remove(&data.db, &cx.user, owner_id, other_id)
        .await?
    {
        Ok(HttpResponse::Ok().json(json!({ "ok": true })))
    } else {
        Err(AppError::NotFound("not linked".into()))
    }
}

// ------------------------------------------------------------------ shared

/// Which form a page shows, and what the requester may do on it.
enum FormMode {
    Create,
    Edit {
        can_edit: bool,
        can_delete: bool,
        actions: Vec<&'static str>,
    },
}

/// Renders the create/edit form. Besides `meta`/`row`/`errors`/`is_new`,
/// the context carries `can_edit` (show inputs + save), `can_delete` (show
/// the delete button) and `actions` (the row actions allowed) — the role's
/// write permission, the `no_*` switches and the model's hooks, already
/// combined — plus `parent` for a nested model.
async fn render_form(
    data: &web::Data<AppData>,
    req: &HttpRequest,
    meta: &'static ModelMeta,
    cx: &Cx,
    row: &Value,
    errors: &[FieldError],
    mode: FormMode,
) -> AppResult {
    let (is_new, can_edit, can_delete, actions) = match mode {
        FormMode::Create => (true, true, false, Vec::new()),
        FormMode::Edit {
            can_edit,
            can_delete,
            actions,
        } => (false, can_edit, can_delete, actions),
    };
    let mut meta_ctx = meta_context(meta, &cx.base_path, can_edit);
    if can_edit {
        add_relation_options(meta, data, &cx.user, &mut meta_ctx).await?;
    }
    let ctx = json!({
        "meta": meta_ctx,
        "row": row,
        "errors": errors,
        "is_new": is_new,
        "can_edit": can_edit,
        "can_delete": can_delete,
        "actions": actions,
        "parent": cx.parent,
    });
    let name = template_name(
        data,
        &format!("{}/form", meta.template_namespace()),
        "fse/form",
    );
    Ok(req.render_tpl(&name, &ctx).await)
}

/// Foreign-key form columns get `options: [{ id, title }]` — the related
/// rows the user may read (permission + scope). Without read permission the
/// options stay `null`, and a submitted id is rejected anyway.
async fn add_relation_options(
    meta: &'static ModelMeta,
    data: &AppData,
    user: &CurrentUser,
    meta_ctx: &mut Value,
) -> AppResult<()> {
    let Some(columns) = meta_ctx["form_columns"].as_array_mut() else {
        return Ok(());
    };
    for col in columns {
        if col["widget"] != "relation" {
            continue;
        }
        let Some(target) = col["name"]
            .as_str()
            .and_then(|name| meta.table.column(name))
            .and_then(|c| c.references.as_ref())
            .and_then(|fk| model_by_struct(&fk.table))
        else {
            continue;
        };
        if !user.has_permission(&target.read_permission()) {
            continue;
        }
        let q = ListQuery {
            page: 1,
            per_page: MAX_OPTIONS,
            ..ListQuery::default()
        };
        let rows = target
            .resource
            .list(&data.db, Access::user(user), &q)
            .await?
            .rows;
        col["options"] = Value::Array(
            rows.iter()
                .map(|r| json!({ "id": r["id"], "title": r[target.ui.title_field] }))
                .collect(),
        );
    }
    Ok(())
}

/// Everything rows need before they reach a page or an API response: the
/// `#[ui(show)]` relations embedded as `{ id, title }` (one level deeper for
/// `show(...)`), and `{col}_display` for `#[ui(format = ...)]` columns in the
/// request's language.
async fn prepare_rows(
    meta: &ModelMeta,
    data: &AppData,
    req: &HttpRequest,
    rows: &mut [Value],
) -> AppResult<()> {
    embed_relations(meta, data, rows).await?;
    let formatted: Vec<_> = meta
        .ui
        .fields
        .iter()
        .filter_map(|f| f.format.map(|kind| (f.name, kind)))
        .collect();
    if formatted.is_empty() {
        return Ok(());
    }
    let lang = data.request_lang(req);
    let currency = data.config.currency.as_deref();
    for row in rows.iter_mut() {
        for (name, kind) in &formatted {
            let value = crate::filters::format(kind, &row[*name], &lang, currency);
            row[format!("{name}_display")] = value;
        }
    }
    Ok(())
}

async fn embed_relations(meta: &ModelMeta, data: &AppData, rows: &mut [Value]) -> AppResult<()> {
    for rel in meta.ui.relations.iter().filter(|r| r.show) {
        let Some(target) = model_by_struct(rel.target) else {
            continue;
        };
        let mut ids: Vec<i64> = rows.iter().filter_map(|r| r[rel.column].as_i64()).collect();
        ids.sort_unstable();
        ids.dedup();
        let refs = target.resource.refs(&data.db, &ids).await?;

        // One level deeper: `show(event)` on `run` embeds `run.event`.
        let mut deeper: BTreeMap<&str, HashMap<i64, super::Ref>> = BTreeMap::new();
        for name in rel.with {
            let Some(next) = target
                .ui
                .relations
                .iter()
                .find(|r| r.field == *name)
                .and_then(|r| model_by_struct(r.target))
            else {
                continue;
            };
            let mut next_ids: Vec<i64> = refs
                .values()
                .flat_map(|r| r.links.iter())
                .filter(|(field, _)| field == name)
                .filter_map(|(_, id)| *id)
                .collect();
            next_ids.sort_unstable();
            next_ids.dedup();
            deeper.insert(name, next.resource.refs(&data.db, &next_ids).await?);
        }

        for row in rows.iter_mut() {
            let embedded = row[rel.column]
                .as_i64()
                .and_then(|id| refs.get(&id).map(|r| (id, r)))
                .map_or(Value::Null, |(id, r)| {
                    let mut obj = json!({ "id": id, "title": r.title });
                    for (name, next_refs) in &deeper {
                        let inner = r
                            .links
                            .iter()
                            .find(|(field, _)| field == name)
                            .and_then(|(_, id)| *id)
                            .and_then(|id| next_refs.get(&id).map(|n| (id, n)))
                            .map_or(Value::Null, |(id, n)| json!({ "id": id, "title": n.title }));
                        obj[*name] = inner;
                    }
                    obj
                });
            row[rel.field] = embedded;
        }
    }
    Ok(())
}

fn api_list_response(result: &ListResult) -> HttpResponse {
    HttpResponse::Ok().json(json!({
        "rows": result.rows,
        "total": result.total,
        "page": result.page,
        "per_page": result.per_page,
        "total_pages": result.total_pages(),
    }))
}

fn redirect(location: &str) -> HttpResponse {
    HttpResponse::Found()
        .append_header((LOCATION, location.to_string()))
        .finish()
}

fn see_other(location: &str) -> HttpResponse {
    HttpResponse::SeeOther()
        .append_header((LOCATION, location.to_string()))
        .finish()
}

/// Where to go after a successful write: the form's `_back` field when it
/// points inside this model's own pages (so a list page with inline forms
/// can return to itself), else `default`. Anything else — another path, a
/// scheme, `//host` — is ignored: `_back` can never become an open redirect.
fn back_or(form: &FormData, base_path: &str, default: String) -> String {
    match form.get("_back").map(|b| b.trim()) {
        Some(back)
            if (back == base_path
                || back.starts_with(&format!("{base_path}/"))
                || back.starts_with(&format!("{base_path}?")))
                && !back.contains("//")
                && !back.contains('\\') =>
        {
            back.to_string()
        }
        _ => default,
    }
}

fn not_found(meta: &ModelMeta) -> AppError {
    AppError::NotFound(format!("{} row not found", meta.table.struct_name))
}

/// Prefer a model-specific template over the theme's generic `fse/*`
/// one — this is the zero-Rust page-override mechanism.
fn template_name(data: &AppData, specific: &str, generic: &str) -> String {
    if data.tera.get_template_names().any(|n| n == specific) {
        specific.to_string()
    } else {
        generic.to_string()
    }
}

/// Query-string → [`ListQuery`]: `search`, `page`, `per_page` (clamped,
/// default `#[model(per_page)]`), `sort`, `dir=desc`, plus each
/// `#[ui(filter)]` column's params (the column name, or
/// `{col}_from`/`{col}_to` for ranges).
fn list_query(meta: &ModelMeta, params: &HashMap<String, String>) -> ListQuery {
    let page = params.get("page").and_then(|p| p.parse().ok()).unwrap_or(1);
    let per_page = params
        .get("per_page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(meta.ui.per_page.unwrap_or(DEFAULT_PER_PAGE))
        .clamp(1, MAX_PER_PAGE);
    ListQuery {
        search: params
            .get("search")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        filters: meta
            .filter_params()
            .into_iter()
            .filter_map(|param| {
                let value = params.get(&param)?.trim();
                (!value.is_empty()).then(|| (param, value.to_string()))
            })
            .collect(),
        sort: params.get("sort").cloned().filter(|s| !s.is_empty()),
        desc: params.get("dir").is_some_and(|d| d == "desc"),
        page,
        per_page,
    }
}

fn list_context(
    meta: &'static ModelMeta,
    base_path: &str,
    q: &ListQuery,
    result: &ListResult,
    can_write: bool,
) -> Value {
    let total_pages = result.total_pages();
    // Every filter param gets an entry ("" = not filtered): Tera errors on
    // a missing map key, so the map must be total for the templates.
    let filters: HashMap<String, &str> = meta
        .filter_params()
        .into_iter()
        .map(|param| {
            let value = q
                .filters
                .iter()
                .find(|(name, _)| *name == param)
                .map_or("", |(_, v)| v.as_str());
            (param, value)
        })
        .collect();
    // Tera (and the fse-ssr proxies) can't do arithmetic, so pagination
    // neighbors are precomputed here. search/sort are plain strings ("" =
    // none) because Tera's `default` filter only covers *missing* keys, not
    // nulls.
    json!({
        "meta": meta_context(meta, base_path, can_write),
        "rows": result.rows,
        "total": result.total,
        // The theme's Pagination component reads `total_count`.
        "total_count": result.total,
        "page": result.page,
        "per_page": result.per_page,
        "total_pages": total_pages,
        "has_prev": result.page > 1,
        "has_next": result.page < total_pages,
        "prev_page": (result.page - 1).max(1),
        "next_page": (result.page + 1).min(total_pages.max(1)),
        "search": q.search.as_deref().unwrap_or(""),
        "sort": q.sort.as_deref().unwrap_or(""),
        "desc": q.desc,
        "filters": filters,
        "can_create": false,
        "parent": Value::Null,
    })
}

/// The model metadata handed to templates — everything a generic page needs
/// to render columns, forms and links. Labels are *not* resolved here;
/// templates translate `models.{table}.fields.{column}` locale keys with a
/// humanized fallback.
fn meta_context(meta: &'static ModelMeta, base_path: &str, can_write: bool) -> Value {
    let column = |c: &fse_schema::ColumnDef| -> Value {
        let f = meta.ui_field(&c.name).expect("every column has a UiField");
        let relation = meta
            .relation_for(&c.name)
            .filter(|r| r.show)
            .map(|r| r.field);
        json!({
            "name": c.name,
            "widget": f.widget.as_str(),
            "options": f.options.map(|options| options()),
            "required": f.required,
            "readonly": f.readonly,
            "nullable": c.nullable,
            "filter": f.filter.map(super::UiFilter::as_str),
            "filter_params": f.filter.map(|kind| kind.params(&c.name)).unwrap_or_default(),
            "format": f.format,
            // The row key to render: the formatted `{col}_display` when the
            // column has a `format`, else the raw column.
            "display": if f.format.is_some() { format!("{}_display", c.name) } else { c.name.clone() },
            "relation": relation,
        })
    };
    // List columns, plus the foreign key of every `#[ui(list)]` relation
    // (rendered as the related row's title).
    let mut list_columns: Vec<&fse_schema::ColumnDef> = meta.list_columns();
    for rel in meta.ui.relations.iter().filter(|r| r.list) {
        if let Some(c) = meta.table.column(rel.column)
            && !list_columns.iter().any(|l| l.name == c.name)
        {
            list_columns.push(c);
        }
    }
    let links: Vec<Value> = meta
        .links()
        .map(|l| json!({ "segment": l.segment(), "table": l.table.name }))
        .collect();
    let children: Vec<Value> = registered_models()
        .iter()
        .filter(|m| !m.ui.disabled && m.parent().is_some_and(|p| std::ptr::eq(p, meta)))
        .map(|m| json!({ "segment": m.segment(), "table": m.table.name }))
        .collect();
    json!({
        "table": meta.table.name,
        "base_path": base_path,
        "can_write": can_write,
        "no_create": meta.ui.no_create,
        "no_edit": meta.ui.no_edit,
        "no_delete": meta.ui.no_delete,
        "public_read": meta.ui.public_read,
        "title_field": meta.ui.title_field,
        "owner": meta.ui.owner,
        "parent_column": meta.ui.parent,
        "actions": meta.ui.actions,
        // Tera has no grouping parentheses, so "any nested pages?" is
        // precomputed rather than left to `children || links` in a template.
        "has_subpages": !children.is_empty() || !links.is_empty(),
        "links": links,
        "children": children,
        "list_columns": list_columns.into_iter().map(column).collect::<Vec<_>>(),
        "form_columns": meta.form_columns().into_iter().map(column).collect::<Vec<_>>(),
        "search_columns": meta.search_columns().into_iter().map(|c| &c.name).collect::<Vec<_>>(),
        "filter_columns": meta.filter_columns().into_iter().map(column).collect::<Vec<_>>(),
    })
}

/// What anonymous (`public_read`) pages get to know about a model: how to
/// render and link rows, nothing about the admin side.
fn public_meta_context(meta: &'static ModelMeta) -> Value {
    let full = meta_context(meta, &meta.base_path(), false);
    let public_only = |cols: &Value| -> Value {
        Value::Array(
            cols.as_array()
                .into_iter()
                .flatten()
                .filter(|c| {
                    c["name"]
                        .as_str()
                        .and_then(|n| meta.ui_field(n))
                        .is_some_and(|f| !f.private)
                })
                .cloned()
                .collect(),
        )
    };
    json!({
        "table": meta.table.name,
        "base_path": format!("/{}", meta.table.name),
        "public_read": meta.ui.public_read,
        "title_field": meta.ui.title_field,
        "list_columns": public_only(&full["list_columns"]),
        "search_columns": full["search_columns"],
        "filter_columns": public_only(&full["filter_columns"]),
    })
}

/// Submitted form values as a JSON object, for re-rendering a rejected form
/// with the user's input intact. Total over the form columns — unchecked
/// checkboxes are absent from the submission, and Tera errors on missing
/// keys — and restricted to them (extra posted fields are dropped).
fn form_values(meta: &ModelMeta, form: &FormData) -> Value {
    let mut obj = serde_json::Map::new();
    for c in meta.form_columns() {
        let value = form.get(&c.name).map_or("", String::as_str);
        obj.insert(c.name.clone(), json!(value));
    }
    Value::Object(obj)
}

/// The create form's initial row: every form column present (Tera errors on
/// missing keys), holding its declared default so the form comes up
/// pre-filled the way the database would fill it.
fn default_row(meta: &ModelMeta) -> Value {
    use fse_schema::DefaultValue;
    let mut obj = serde_json::Map::new();
    for c in meta.form_columns() {
        let value = match &c.default {
            Some(DefaultValue::Int(i)) => json!(i),
            Some(DefaultValue::Float(f)) => json!(f),
            Some(DefaultValue::Text(s)) => json!(s),
            Some(DefaultValue::Bool(b)) => json!(b),
            Some(DefaultValue::Now) | None => Value::Null,
        };
        obj.insert(c.name.clone(), value);
    }
    Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_stays_inside_the_model() {
        let form = |v: &str| -> FormData { [("_back".to_string(), v.to_string())].into() };
        let d = || "/notes/1".to_string();
        assert_eq!(back_or(&form("/notes"), "/notes", d()), "/notes");
        assert_eq!(
            back_or(&form("/notes?page=2"), "/notes", d()),
            "/notes?page=2"
        );
        assert_eq!(back_or(&form("/notes/7"), "/notes", d()), "/notes/7");
        for evil in [
            "https://evil.test",
            "//evil.test",
            "/notesx",
            "/notes//evil.test",
            "/other",
            "/notes/\\evil",
        ] {
            assert_eq!(back_or(&form(evil), "/notes", d()), "/notes/1", "{evil}");
        }
        assert_eq!(back_or(&FormData::new(), "/notes", d()), "/notes/1");
    }
}
