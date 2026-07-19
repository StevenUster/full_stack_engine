//! End-to-end check of `#[derive(Model)]`: this test crate plays the role of
//! an app — it derives `Model` on structs and asserts that the metadata shows
//! up in the runtime registry with conventions resolved. No database is
//! involved: `Model` alone emits no queries (that's `derive(Table)`'s job).

use chrono::NaiveDateTime;
use fse_orm::DbEnum;
use full_stack_engine::models::{self, UiWidget};
use full_stack_engine::prelude::Model;

#[derive(DbEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum ArticleStatus {
    Draft,
    Published,
}

#[derive(Model)]
#[model(permission = "content", path = "article-admin", public_read = slug, api, no_delete)]
#[allow(dead_code)]
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

/// A model with no `#[model]`/`#[ui]` attributes at all — everything must
/// resolve from conventions.
#[derive(Model)]
#[allow(dead_code)]
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
    assert_eq!(m.base_path(), "/notes");
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
fn registry_lists_both_models_sorted() {
    let names: Vec<&str> = models::registered_models()
        .iter()
        .map(|m| m.table.name.as_str())
        .collect();
    assert!(names.windows(2).all(|w| w[0] <= w[1]), "sorted: {names:?}");
    assert!(names.contains(&"articles"));
    assert!(names.contains(&"notes"));
}
