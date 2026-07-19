//! End-to-end check of `#[model(...)]`: this test crate plays the role of an
//! app — one annotation per struct, and the metadata must show up in the
//! runtime registry with conventions resolved. The macro also expands to
//! `#[derive(Table)]`, whose generated sqlx queries are compile-time checked
//! against the scratch database built by build.rs from these very structs —
//! so this file proves the whole single-annotation loop.

#![allow(dead_code)]

use chrono::NaiveDateTime;
use fse_orm::DbEnum;
use full_stack_engine::models::{self, UiWidget};
use full_stack_engine::prelude::model;

#[derive(DbEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum ArticleStatus {
    Draft,
    Published,
}

#[model(permission = "content", path = "article-admin", public_read = slug, api, no_delete)]
struct Article {
    id: i64,
    #[ui(list, search)]
    title: String,
    #[orm(unique)]
    #[ui(list)]
    slug: String,
    #[ui(textarea)]
    body: Option<String>,
    #[orm(default = "draft")]
    #[ui(list, filter)]
    status: ArticleStatus,
    #[orm(default = now)]
    #[ui(list)]
    created_at: NaiveDateTime,
}

/// A model with no arguments and no `#[ui]` attributes at all — everything
/// must resolve from conventions.
#[model]
struct Note {
    id: i64,
    text: String,
    pinned: bool,
}

#[test]
fn article_is_registered_with_parsed_table() {
    let m = models::model("articles").expect("Article registered via inventory");
    assert_eq!(m.table.struct_name, "Article");
    assert_eq!(m.table.columns.len(), 6);
    assert!(m.table.column("slug").unwrap().unique);
    assert!(m.table.column("id").unwrap().primary_key);
}

#[test]
fn article_model_options_apply() {
    let m = models::model("articles").unwrap();
    assert_eq!(m.permission_base(), "content");
    assert_eq!(m.read_permission(), "content.read");
    assert_eq!(m.write_permission(), "content.write");
    assert_eq!(m.base_path(), "/article-admin");
    assert_eq!(m.ui.public_read, Some("slug"));
    assert!(m.ui.api);
    assert!(m.ui.no_delete);
    assert!(!m.ui.no_create);
    assert!(!m.ui.disabled);
}

#[test]
fn article_ui_fields_resolve() {
    let m = models::model("articles").unwrap();

    let list: Vec<&str> = m.list_columns().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(list, ["title", "slug", "status", "created_at"]);

    let search: Vec<&str> = m.search_columns().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(search, ["title"]);

    let filter: Vec<&str> = m.filter_columns().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(filter, ["status"]);

    // Forms: no id (pk), no created_at (default = now).
    let form: Vec<&str> = m.form_columns().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(form, ["title", "slug", "body", "status"]);

    assert_eq!(m.title_column().name, "title");

    assert_eq!(m.ui_field("body").unwrap().widget, UiWidget::Textarea);
    let status = m.ui_field("status").unwrap();
    assert_eq!(status.widget, UiWidget::Select);
    let options = (status.options.expect("select offers the DbEnum variants"))();
    assert_eq!(options, ["draft", "published"]);
}

#[test]
fn note_resolves_purely_from_conventions() {
    let m = models::model("notes").expect("Note registered via inventory");
    assert_eq!(m.permission_base(), "notes");
    assert_eq!(m.base_path(), "/admin/notes");
    assert_eq!(m.ui.public_read, None);
    assert!(!m.ui.api);

    // No #[ui(list)] anywhere: all visible non-pk columns.
    let list: Vec<&str> = m.list_columns().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(list, ["text", "pinned"]);

    assert_eq!(m.title_column().name, "text");
    assert_eq!(m.ui_field("pinned").unwrap().widget, UiWidget::Checkbox);
    assert_eq!(m.ui_field("text").unwrap().widget, UiWidget::Text);
}

#[test]
fn model_expands_to_the_orm_table_derive() {
    // The Table derive ran: its generated API exists and carries the parsed
    // schema. Its literal SQL (fetch, fetch_by_slug, ...) was compile-time
    // checked against the build.rs database — a broken expansion would have
    // failed this build, not this assertion.
    assert_eq!(Article::TABLE, "articles");
    assert_eq!(Note::TABLE, "notes");

    // Debug + Clone were auto-derived.
    let note = Note {
        id: 1,
        text: "hi".into(),
        pinned: false,
    };
    let _ = format!("{:?}", note.clone());
}

/// The generated `ModelResource` drives real CRUD against the build.rs
/// database: create (with validation + uniqueness), get, list (search +
/// filter), update, delete. One test so the shared db file has no ordering
/// races.
#[tokio::test]
async fn resource_crud_round_trip() {
    let db = sqlx::SqlitePool::connect(env!("DATABASE_URL")).await.unwrap();
    Article::delete_where().execute(&db).await.unwrap();
    let r = models::model("articles").unwrap().resource;

    let form = |pairs: &[(&str, &str)]| -> models::FormData {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    };

    // Missing title + bad status: every error collected, nothing inserted.
    let errors = r
        .create(&db, &form(&[("slug", "a"), ("status", "nope")]))
        .await
        .unwrap()
        .unwrap_err();
    let codes: Vec<(&str, &str)> = errors.iter().map(|e| (e.field, e.code)).collect();
    assert!(codes.contains(&("title", "required")), "{codes:?}");
    assert!(codes.contains(&("status", "invalid_option")), "{codes:?}");

    // Valid create; body empty -> NULL.
    let id = r
        .create(
            &db,
            &form(&[("title", "Hello World"), ("slug", "hello"), ("status", "draft")]),
        )
        .await
        .unwrap()
        .unwrap();

    // Duplicate slug -> not_unique.
    let errors = r
        .create(
            &db,
            &form(&[("title", "Other"), ("slug", "hello"), ("status", "draft")]),
        )
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(errors[0].field, "slug");
    assert_eq!(errors[0].code, "not_unique");

    // get: visible columns as JSON, enum as stored string, body NULL.
    let row = r.get(&db, id).await.unwrap().unwrap();
    assert_eq!(row["title"], "Hello World");
    assert_eq!(row["status"], "draft");
    assert!(row["body"].is_null());
    assert_eq!(row["id"], id);

    // public_read = slug.
    let row = r.get_by_public(&db, "hello").await.unwrap().unwrap();
    assert_eq!(row["id"], id);
    assert!(r.get_by_public(&db, "nope").await.unwrap().is_none());

    // Second row to make list filters observable.
    r.create(
        &db,
        &form(&[("title", "Zebra"), ("slug", "zebra"), ("status", "published")]),
    )
    .await
    .unwrap()
    .unwrap();

    let base = models::ListQuery {
        page: 1,
        per_page: 10,
        ..Default::default()
    };

    let all = r.list(&db, &base).await.unwrap();
    assert_eq!(all.total, 2);
    assert_eq!(all.total_pages(), 1);

    let q = models::ListQuery {
        search: Some("hello".into()),
        ..base.clone()
    };
    let searched = r.list(&db, &q).await.unwrap();
    assert_eq!(searched.total, 1);
    assert_eq!(searched.rows[0]["slug"], "hello");

    let q = models::ListQuery {
        filters: vec![("status".into(), "published".into())],
        ..base.clone()
    };
    let filtered = r.list(&db, &q).await.unwrap();
    assert_eq!(filtered.total, 1);
    assert_eq!(filtered.rows[0]["slug"], "zebra");

    // Unknown filter/sort names are ignored, not errors.
    let q = models::ListQuery {
        filters: vec![("nope".into(), "x".into())],
        sort: Some("nope".into()),
        ..base.clone()
    };
    assert_eq!(r.list(&db, &q).await.unwrap().total, 2);

    let q = models::ListQuery {
        sort: Some("title".into()),
        ..base.clone()
    };
    let sorted = r.list(&db, &q).await.unwrap();
    assert_eq!(sorted.rows[0]["title"], "Hello World");

    // update: same slug on the row itself is fine; new values land.
    r.update(
        &db,
        id,
        &form(&[("title", "Hello Again"), ("slug", "hello"), ("status", "published")]),
    )
    .await
    .unwrap()
    .unwrap();
    let row = r.get(&db, id).await.unwrap().unwrap();
    assert_eq!(row["title"], "Hello Again");
    assert_eq!(row["status"], "published");

    // update: stealing another row's slug is rejected.
    let errors = r
        .update(&db, id, &form(&[("title", "X"), ("slug", "zebra"), ("status", "draft")]))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(errors[0].code, "not_unique");

    // delete.
    assert_eq!(r.delete(&db, id).await.unwrap(), 1);
    assert_eq!(r.delete(&db, id).await.unwrap(), 0);
    assert!(r.get(&db, id).await.unwrap().is_none());
}

#[test]
fn form_fields_are_the_macro_computed_set() {
    let m = models::model("articles").unwrap();
    // No id (pk), no created_at (default = now).
    assert_eq!(m.ui.form_fields, ["title", "slug", "body", "status"]);
    let names: Vec<&str> = m.form_columns().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["title", "slug", "body", "status"]);
}

#[test]
fn registry_lists_both_models_sorted() {
    let names: Vec<&str> = models::registered_models()
        .iter()
        .map(|m| m.table.name.as_str())
        .collect();
    assert!(names.windows(2).all(|w| w[0] <= w[1]), "sorted: {names:?}");
    assert!(names.contains(&"articles"));
    assert!(names.contains(&"notes"));
}
