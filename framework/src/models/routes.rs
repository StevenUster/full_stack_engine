//! Generic CRUD HTTP handlers, mounted at boot for every `#[model]` struct
//! via [`crate::FrameworkApp::models`].
//!
//! Overriding: app routes are registered *before* these (actix matches in
//! registration order), so a same-path route in the app's `configure` simply
//! shadows the generated one. Templates are chosen per model — a template
//! named after the model's path segment (e.g. `admin/posts`,
//! `admin/posts/form`, `posts`, `posts/detail`) wins over the theme's
//! generic `fse/*` templates, so one page can be overridden with zero
//! Rust.
//!
//! Every handler enforces the model's conventional permissions through
//! [`AuthUser`]: `<base>.read` for pages that show data, `<base>.write` for
//! anything that changes it. `public_read` pages and their JSON API carry no
//! auth by design.

use std::collections::HashMap;

use actix_web::http::header::LOCATION;
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};

use super::{FieldError, FormData, ListQuery, ListResult, ModelMeta, registered_models};
use crate::auth::AuthUser;
use crate::error::{AppError, AppResult};
use crate::structs::Role;
use crate::{AppData, RenderTplExt};

const DEFAULT_PER_PAGE: i64 = 20;
const MAX_PER_PAGE: i64 = 100;

/// Mounts the generated routes for every registered, non-`disabled` model.
/// `R` is the app's role enum — permission checks run against it. Called by
/// [`crate::FrameworkApp::models`]; public so tests (and unusual setups) can
/// apply it to a raw `ServiceConfig`.
///
/// # Panics
///
/// Panics when two models claim the same table name (see
/// [`registered_models`]) — a boot-time check by design.
pub fn mount_all<R: Role>(cfg: &mut web::ServiceConfig) {
    for meta in registered_models() {
        if !meta.ui.disabled {
            mount_model::<R>(cfg, meta);
        }
    }
}

#[allow(clippy::too_many_lines)]
fn mount_model<R: Role>(cfg: &mut web::ServiceConfig, meta: &'static ModelMeta) {
    let base = meta.base_path();

    cfg.route(
        &base,
        web::get().to(
            move |data: web::Data<AppData>,
                  req: HttpRequest,
                  user: AuthUser<R>,
                  query: web::Query<HashMap<String, String>>| {
                list_page(meta, data, req, user, query)
            },
        ),
    );

    // `/create` before `/{id}` so the literal segment is matched first.
    if !meta.ui.no_create {
        cfg.route(
            &format!("{base}/create"),
            web::get().to(
                move |data: web::Data<AppData>, req: HttpRequest, user: AuthUser<R>| {
                    create_form(meta, data, req, user)
                },
            ),
        );
        cfg.route(
            &format!("{base}/create"),
            web::post().to(
                move |data: web::Data<AppData>,
                      req: HttpRequest,
                      user: AuthUser<R>,
                      form: web::Form<FormData>| {
                    create_submit(meta, data, req, user, form)
                },
            ),
        );
    }

    cfg.route(
        &format!("{base}/{{id}}"),
        web::get().to(
            move |data: web::Data<AppData>,
                  req: HttpRequest,
                  user: AuthUser<R>,
                  id: web::Path<i64>| { edit_form(meta, data, req, user, id) },
        ),
    );
    if !meta.ui.no_edit {
        cfg.route(
            &format!("{base}/{{id}}"),
            web::post().to(
                move |data: web::Data<AppData>,
                      req: HttpRequest,
                      user: AuthUser<R>,
                      id: web::Path<i64>,
                      form: web::Form<FormData>| {
                    update_submit(meta, data, req, user, id, form)
                },
            ),
        );
    }
    if !meta.ui.no_delete {
        cfg.route(
            &format!("{base}/{{id}}"),
            web::delete().to(
                move |data: web::Data<AppData>, user: AuthUser<R>, id: web::Path<i64>| {
                    delete_row(meta, data, user, id)
                },
            ),
        );
    }

    if meta.ui.public_read.is_some() {
        let table = &meta.table.name;
        cfg.route(
            &format!("/{table}"),
            web::get().to(
                move |data: web::Data<AppData>,
                      req: HttpRequest,
                      query: web::Query<HashMap<String, String>>| {
                    public_list(meta, data, req, query)
                },
            ),
        );
        cfg.route(
            &format!("/{table}/{{key}}"),
            web::get().to(
                move |data: web::Data<AppData>, req: HttpRequest, key: web::Path<String>| {
                    public_detail(meta, data, req, key)
                },
            ),
        );
    }

    if meta.ui.api {
        let table = &meta.table.name;
        if meta.ui.public_read.is_some() {
            cfg.route(
                &format!("/api/{table}"),
                web::get().to(
                    move |data: web::Data<AppData>, query: web::Query<HashMap<String, String>>| {
                        api_list(meta, data, query)
                    },
                ),
            );
            cfg.route(
                &format!("/api/{table}/{{key}}"),
                web::get().to(move |data: web::Data<AppData>, key: web::Path<String>| {
                    api_detail_public(meta, data, key)
                }),
            );
        } else {
            cfg.route(
                &format!("/api/{table}"),
                web::get().to(
                    move |data: web::Data<AppData>,
                          user: AuthUser<R>,
                          query: web::Query<HashMap<String, String>>| {
                        api_list_authed(meta, data, user, query)
                    },
                ),
            );
            cfg.route(
                &format!("/api/{table}/{{id}}"),
                web::get().to(
                    move |data: web::Data<AppData>, user: AuthUser<R>, id: web::Path<i64>| {
                        api_detail_authed(meta, data, user, id)
                    },
                ),
            );
        }
    }
}

// ---------------------------------------------------------------- handlers

async fn list_page<R: Role>(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
    query: web::Query<HashMap<String, String>>,
) -> AppResult {
    user.require_permission(&meta.read_permission())?;
    let q = list_query(meta, &query);
    let result = meta.resource.list(&data.db, &q).await?;
    let can_write = user.claims.role.has_permission(&meta.write_permission());
    let ctx = list_context(meta, &q, &result, can_write);
    let name = template_name(&data, &admin_segment(meta), "fse/list");
    Ok(req.render_tpl(&name, &ctx).await)
}

async fn create_form<R: Role>(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
) -> AppResult {
    user.require_permission(&meta.write_permission())?;
    render_form(&data, &req, meta, &default_row(meta), &[], true).await
}

async fn create_submit<R: Role>(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
    form: web::Form<FormData>,
) -> AppResult {
    user.require_permission(&meta.write_permission())?;
    match meta.resource.create(&data.db, &form).await? {
        Ok(id) => Ok(redirect(&format!(
            "{}{}/{id}",
            data.lang_prefix(&req),
            meta.base_path()
        ))),
        Err(errors) => {
            render_form(&data, &req, meta, &form_values(meta, &form), &errors, true).await
        }
    }
}

async fn edit_form<R: Role>(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
    id: web::Path<i64>,
) -> AppResult {
    user.require_permission(&meta.read_permission())?;
    let row = meta
        .resource
        .get(&data.db, *id)
        .await?
        .ok_or_else(|| not_found(meta))?;
    render_form(&data, &req, meta, &row, &[], false).await
}

async fn update_submit<R: Role>(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    user: AuthUser<R>,
    id: web::Path<i64>,
    form: web::Form<FormData>,
) -> AppResult {
    user.require_permission(&meta.write_permission())?;
    let id = *id;
    if meta.resource.get(&data.db, id).await?.is_none() {
        return Err(not_found(meta));
    }
    match meta.resource.update(&data.db, id, &form).await? {
        Ok(()) => Ok(redirect(&format!(
            "{}{}/{id}",
            data.lang_prefix(&req),
            meta.base_path()
        ))),
        Err(errors) => {
            let mut row = form_values(meta, &form);
            row["id"] = json!(id);
            render_form(&data, &req, meta, &row, &errors, false).await
        }
    }
}

async fn delete_row<R: Role>(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    user: AuthUser<R>,
    id: web::Path<i64>,
) -> AppResult {
    user.require_permission(&meta.write_permission())?;
    let deleted = meta.resource.delete(&data.db, *id).await?;
    if deleted == 0 {
        return Err(not_found(meta));
    }
    Ok(HttpResponse::Ok().json(json!({ "ok": true })))
}

async fn public_list(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    query: web::Query<HashMap<String, String>>,
) -> AppResult {
    let q = list_query(meta, &query);
    let result = meta.resource.list(&data.db, &q).await?;
    let ctx = list_context(meta, &q, &result, false);
    let name = template_name(&data, &meta.table.name, "fse/public-list");
    Ok(req.render_tpl(&name, &ctx).await)
}

async fn public_detail(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    req: HttpRequest,
    key: web::Path<String>,
) -> AppResult {
    let row = meta
        .resource
        .get_by_public(&data.db, &key)
        .await?
        .ok_or_else(|| not_found(meta))?;
    let ctx = json!({ "meta": meta_context(meta, false), "row": row });
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
    query: web::Query<HashMap<String, String>>,
) -> AppResult {
    let q = list_query(meta, &query);
    let result = meta.resource.list(&data.db, &q).await?;
    Ok(api_list_response(&result))
}

async fn api_list_authed<R: Role>(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    user: AuthUser<R>,
    query: web::Query<HashMap<String, String>>,
) -> AppResult {
    user.require_permission(&meta.read_permission())?;
    let q = list_query(meta, &query);
    let result = meta.resource.list(&data.db, &q).await?;
    Ok(api_list_response(&result))
}

async fn api_detail_public(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    key: web::Path<String>,
) -> AppResult {
    let row = meta
        .resource
        .get_by_public(&data.db, &key)
        .await?
        .ok_or_else(|| not_found(meta))?;
    Ok(HttpResponse::Ok().json(row))
}

async fn api_detail_authed<R: Role>(
    meta: &'static ModelMeta,
    data: web::Data<AppData>,
    user: AuthUser<R>,
    id: web::Path<i64>,
) -> AppResult {
    user.require_permission(&meta.read_permission())?;
    let row = meta
        .resource
        .get(&data.db, *id)
        .await?
        .ok_or_else(|| not_found(meta))?;
    Ok(HttpResponse::Ok().json(row))
}

// ------------------------------------------------------------------ shared

async fn render_form(
    data: &web::Data<AppData>,
    req: &HttpRequest,
    meta: &'static ModelMeta,
    row: &Value,
    errors: &[FieldError],
    is_new: bool,
) -> AppResult {
    let ctx = json!({
        "meta": meta_context(meta, true),
        "row": row,
        "errors": errors,
        "is_new": is_new,
    });
    let name = template_name(data, &format!("{}/form", admin_segment(meta)), "fse/form");
    Ok(req.render_tpl(&name, &ctx).await)
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

fn not_found(meta: &ModelMeta) -> AppError {
    AppError::NotFound(format!("{} row not found", meta.table.struct_name))
}

/// The admin template namespace: the base path without its leading slash
/// (`admin/posts`, or the explicit `path` segment).
fn admin_segment(meta: &ModelMeta) -> String {
    meta.base_path().split_off(1)
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

/// Query-string → [`ListQuery`]: `search`, `page`, `per_page` (clamped),
/// `sort`, `dir=desc`, plus one param per `#[ui(filter)]` column.
fn list_query(meta: &ModelMeta, params: &HashMap<String, String>) -> ListQuery {
    let page = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1);
    let per_page = params
        .get("per_page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PER_PAGE)
        .clamp(1, MAX_PER_PAGE);
    ListQuery {
        search: params
            .get("search")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        filters: meta
            .filter_columns()
            .iter()
            .filter_map(|c| {
                params
                    .get(&c.name)
                    .filter(|v| !v.is_empty())
                    .map(|v| (c.name.clone(), v.clone()))
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
    q: &ListQuery,
    result: &ListResult,
    can_write: bool,
) -> Value {
    let total_pages = result.total_pages();
    // Every filter column gets an entry ("" = not filtered): Tera errors on
    // a missing map key, so the map must be total for the templates.
    let filters: HashMap<&str, &str> = meta
        .filter_columns()
        .into_iter()
        .map(|c| {
            let value = q
                .filters
                .iter()
                .find(|(name, _)| *name == c.name)
                .map_or("", |(_, v)| v.as_str());
            (c.name.as_str(), value)
        })
        .collect();
    // Tera (and the fse-ssr proxies) can't do arithmetic, so pagination
    // neighbors are precomputed here. search/sort are plain strings ("" =
    // none) because Tera's `default` filter only covers *missing* keys, not
    // nulls.
    json!({
        "meta": meta_context(meta, can_write),
        "rows": result.rows,
        "total": result.total,
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
    })
}

/// The model metadata handed to templates — everything a generic page needs
/// to render columns, forms and links. Labels are *not* resolved here;
/// templates translate `models.{table}.fields.{column}` locale keys with a
/// humanized fallback.
fn meta_context(meta: &'static ModelMeta, can_write: bool) -> Value {
    let column = |c: &fse_schema::ColumnDef| -> Value {
        let f = meta
            .ui_field(&c.name)
            .expect("every column has a UiField");
        json!({
            "name": c.name,
            "widget": f.widget.as_str(),
            "options": f.options.map(|options| options()),
            "required": !c.nullable && c.default.is_none(),
            "readonly": f.readonly,
            "nullable": c.nullable,
        })
    };
    json!({
        "table": meta.table.name,
        "base_path": meta.base_path(),
        "can_write": can_write,
        "no_create": meta.ui.no_create,
        "no_edit": meta.ui.no_edit,
        "no_delete": meta.ui.no_delete,
        "public_read": meta.ui.public_read,
        "title_field": meta.title_column().name,
        "list_columns": meta.list_columns().into_iter().map(column).collect::<Vec<_>>(),
        "form_columns": meta.form_columns().into_iter().map(column).collect::<Vec<_>>(),
        "search_columns": meta.search_columns().into_iter().map(|c| &c.name).collect::<Vec<_>>(),
        "filter_columns": meta.filter_columns().into_iter().map(column).collect::<Vec<_>>(),
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
