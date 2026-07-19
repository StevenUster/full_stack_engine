//! End-to-end tests for the schema layer: source → model → DDL → diffs.
//! Fixtures are written the way an app's `src/tables/*.rs` files would be.

use fse_schema::parse::{parse_sources, pluralize, to_snake_case};
use fse_schema::snapshot::{schema_from_json, schema_to_json};
use fse_schema::sql::create_table_sql;
use fse_schema::{DefaultValue, OnDelete, Schema, SqlType, diff_schemas};

fn schema_of(sources: &[(&str, &str)]) -> Schema {
    let owned: Vec<(String, String)> = sources
        .iter()
        .map(|(n, c)| (n.to_string(), c.to_string()))
        .collect();
    parse_sources(&owned).unwrap_or_else(|e| panic!("parse failed: {e}"))
}

const EVENT: &str = r#"
#[derive(Table)]
pub struct Event {
    pub id: i64,
    pub name: String,
}
"#;

const PRODUCT_V1: &str = r#"
#[derive(DbEnum)]
pub enum ProductStatus {
    Draft,
    Published,
    Archived,
}

#[derive(Table)]
pub struct Product {
    pub id: i64,
    #[orm(unique)]
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    #[orm(default = 0.0)]
    pub price: f64,
    #[orm(default = "draft")]
    pub status: ProductStatus,
    #[orm(references(Event, on_delete = cascade))]
    pub event_id: i64,
    #[orm(default = now)]
    pub created_at: NaiveDateTime,
    #[orm(json)]
    pub dimensions: Option<Dimensions>,
}
"#;

fn v1() -> Schema {
    schema_of(&[("event.rs", EVENT), ("product.rs", PRODUCT_V1)])
}

#[test]
fn parses_the_full_model() {
    let schema = v1();
    assert_eq!(schema.enums.len(), 1);
    assert_eq!(schema.enums[0].values, ["draft", "published", "archived"]);

    // Tables are sorted by name for deterministic snapshots.
    assert_eq!(schema.tables[0].name, "events");
    let p = schema.table("products").unwrap();
    assert_eq!(p.struct_name, "Product");
    assert!(p.auto_id());

    let id = p.column("id").unwrap();
    assert!(id.primary_key && !id.nullable);

    let slug = p.column("slug").unwrap();
    assert!(slug.unique);
    assert_eq!(slug.ty, SqlType::Text);

    let description = p.column("description").unwrap();
    assert!(description.nullable);

    let price = p.column("price").unwrap();
    assert_eq!(price.default, Some(DefaultValue::Float(0.0)));

    let status = p.column("status").unwrap();
    assert!(status.is_enum);
    assert_eq!(
        status.check_in.as_deref().unwrap(),
        ["draft", "published", "archived"]
    );
    assert_eq!(status.default, Some(DefaultValue::Text("draft".into())));

    // FK target resolved from struct name to table name.
    let fk = p.column("event_id").unwrap().references.as_ref().unwrap();
    assert_eq!(fk.table, "events");
    assert_eq!(fk.column, "id");
    assert_eq!(fk.on_delete, Some(OnDelete::Cascade));

    let created = p.column("created_at").unwrap();
    assert_eq!(created.ty, SqlType::Timestamp);
    assert_eq!(created.default, Some(DefaultValue::Now));

    let dims = p.column("dimensions").unwrap();
    assert!(dims.json && dims.nullable);
    assert_eq!(dims.ty, SqlType::Text);
}

#[test]
fn generates_create_table_sql() {
    let schema = v1();
    let sql = create_table_sql(schema.table("products").unwrap());
    assert_eq!(
        sql,
        "CREATE TABLE \"products\" (\n    \
             \"id\" INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,\n    \
             \"slug\" TEXT NOT NULL UNIQUE,\n    \
             \"name\" TEXT NOT NULL,\n    \
             \"description\" TEXT,\n    \
             \"price\" REAL NOT NULL DEFAULT 0,\n    \
             \"status\" TEXT NOT NULL DEFAULT 'draft' CHECK (\"status\" IN ('draft', 'published', 'archived')),\n    \
             \"event_id\" INTEGER NOT NULL,\n    \
             \"created_at\" TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,\n    \
             \"dimensions\" TEXT,\n    \
             FOREIGN KEY (\"event_id\") REFERENCES \"events\"(\"id\") ON DELETE CASCADE\n\
         );"
    );
}

#[test]
fn identical_schemas_produce_no_migration() {
    assert!(diff_schemas(&v1(), &v1()).unwrap().is_none());
}

#[test]
fn new_table_is_created_and_removed_table_dropped() {
    let old = schema_of(&[("event.rs", EVENT)]);
    let new = v1();

    let m = diff_schemas(&old, &new).unwrap().unwrap();
    assert!(m.sql.starts_with("CREATE TABLE \"products\" ("));
    assert!(!m.destructive);

    let back = diff_schemas(&new, &old).unwrap().unwrap();
    assert_eq!(back.sql, "DROP TABLE \"products\";\n");
    assert!(back.destructive);
}

#[test]
fn simple_added_column_uses_alter_table() {
    let new_src = PRODUCT_V1.replace(
        "pub name: String,",
        "pub name: String,\n    pub archived_at: Option<NaiveDateTime>,",
    );
    let m = diff_schemas(
        &v1(),
        &schema_of(&[("event.rs", EVENT), ("product.rs", &new_src)]),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        m.sql,
        "ALTER TABLE \"products\" ADD COLUMN \"archived_at\" TIMESTAMP;\n"
    );
    assert!(!m.destructive && !m.needs_manual_edit);
}

#[test]
fn renamed_column_uses_rename_column() {
    let new_src = PRODUCT_V1.replace(
        "pub name: String,",
        "#[orm(renamed_from = \"name\")]\n    pub title: String,",
    );
    let m = diff_schemas(
        &v1(),
        &schema_of(&[("event.rs", EVENT), ("product.rs", &new_src)]),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        m.sql,
        "ALTER TABLE \"products\" RENAME COLUMN \"name\" TO \"title\";\n"
    );

    // Attribute left in place after the migration ran: no-op, not a re-rename.
    let renamed = schema_of(&[("event.rs", EVENT), ("product.rs", &new_src)]);
    assert!(diff_schemas(&renamed, &renamed).unwrap().is_none());
}

#[test]
fn dropped_column_is_destructive() {
    let new_src = PRODUCT_V1.replace("pub description: Option<String>,", "");
    let m = diff_schemas(
        &v1(),
        &schema_of(&[("event.rs", EVENT), ("product.rs", &new_src)]),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        m.sql,
        "ALTER TABLE \"products\" DROP COLUMN \"description\";\n"
    );
    assert!(m.destructive);
}

#[test]
fn type_change_rebuilds_the_table() {
    let new_src = PRODUCT_V1.replace(
        "#[orm(default = 0.0)]\n    pub price: f64,",
        "#[orm(default = 0)]\n    pub price: i64,",
    );
    let m = diff_schemas(
        &v1(),
        &schema_of(&[("event.rs", EVENT), ("product.rs", &new_src)]),
    )
    .unwrap()
    .unwrap();
    assert!(m.sql.contains("CREATE TABLE \"products_new\" ("));
    assert!(m.sql.contains("\"price\" INTEGER NOT NULL DEFAULT 0"));
    // Existing rows are copied across, column list intact.
    assert!(m.sql.contains(
        "INSERT INTO \"products_new\" (\"id\", \"slug\", \"name\", \"description\", \"price\", \"status\", \"event_id\", \"created_at\", \"dimensions\")"
    ));
    assert!(m.sql.contains("DROP TABLE \"products\";"));
    assert!(
        m.sql
            .contains("ALTER TABLE \"products_new\" RENAME TO \"products\";")
    );
    assert!(!m.destructive && !m.needs_manual_edit);
}

#[test]
fn new_not_null_column_without_default_needs_manual_backfill() {
    let new_src = PRODUCT_V1.replace(
        "pub name: String,",
        "pub name: String,\n    pub sku: String,",
    );
    let m = diff_schemas(
        &v1(),
        &schema_of(&[("event.rs", EVENT), ("product.rs", &new_src)]),
    )
    .unwrap()
    .unwrap();
    assert!(m.needs_manual_edit);
    assert!(m.sql.contains("/* TODO: backfill NOT NULL column sku */"));
}

#[test]
fn added_unique_column_forces_a_rebuild() {
    let new_src = PRODUCT_V1.replace(
        "pub name: String,",
        "pub name: String,\n    #[orm(unique)]\n    pub sku: Option<String>,",
    );
    let m = diff_schemas(
        &v1(),
        &schema_of(&[("event.rs", EVENT), ("product.rs", &new_src)]),
    )
    .unwrap()
    .unwrap();
    assert!(m.sql.contains("CREATE TABLE \"products_new\" ("));
    assert!(m.sql.contains("\"sku\" TEXT UNIQUE"));
}

#[test]
fn enum_value_change_rebuilds_with_new_check() {
    let new_src = PRODUCT_V1.replace("Archived,", "Archived,\n    Discontinued,");
    let m = diff_schemas(
        &v1(),
        &schema_of(&[("event.rs", EVENT), ("product.rs", &new_src)]),
    )
    .unwrap()
    .unwrap();
    assert!(
        m.sql
            .contains("CHECK (\"status\" IN ('draft', 'published', 'archived', 'discontinued'))")
    );
}

#[test]
fn composite_primary_key_renders_as_table_constraint() {
    let src = r#"
#[derive(Table)]
pub struct EventManager {
    #[orm(primary_key, references(Event, on_delete = cascade))]
    pub event_id: i64,
    #[orm(primary_key)]
    pub user_id: i64,
}
"#;
    let schema = schema_of(&[("event.rs", EVENT), ("event_manager.rs", src)]);
    let sql = create_table_sql(schema.table("event_managers").unwrap());
    assert!(sql.contains("PRIMARY KEY (\"event_id\", \"user_id\")"));
    assert!(!sql.contains("AUTOINCREMENT"));
}

#[test]
fn text_and_index_attributes() {
    let src = r#"
#[derive(Table)]
pub struct Account {
    pub id: i64,
    #[orm(text, default = "none")]
    pub role: AppRole,
    #[orm(index)]
    pub team_id: i64,
}
"#;
    let schema = schema_of(&[("account.rs", src)]);
    let t = schema.table("accounts").unwrap();

    // #[orm(text)]: TEXT via as_str()/FromStr, no CHECK constraint.
    let role = t.column("role").unwrap();
    assert!(role.is_enum && role.check_in.is_none());
    assert_eq!(role.ty, SqlType::Text);
    let sql = create_table_sql(t);
    assert!(sql.contains("\"role\" TEXT NOT NULL DEFAULT 'none'"));
    assert!(!sql.contains("CHECK"));

    // #[orm(index)]: separate CREATE INDEX statement, emitted on create.
    let m = diff_schemas(&Schema::default(), &schema).unwrap().unwrap();
    assert!(
        m.sql
            .contains("CREATE INDEX \"idx_accounts_team_id\" ON \"accounts\" (\"team_id\");")
    );

    // Removing the index later is a plain DROP INDEX, not a rebuild.
    let without = schema_of(&[("account.rs", &src.replace("#[orm(index)]\n    ", ""))]);
    let m = diff_schemas(&schema, &without).unwrap().unwrap();
    assert_eq!(m.sql, "DROP INDEX IF EXISTS \"idx_accounts_team_id\";\n");
    let back = diff_schemas(&without, &schema).unwrap().unwrap();
    assert_eq!(
        back.sql,
        "CREATE INDEX \"idx_accounts_team_id\" ON \"accounts\" (\"team_id\");\n"
    );
}

#[test]
fn struct_level_unique_and_index_attributes() {
    let src = r#"
#[derive(Table)]
#[orm(unique(user_id, run_id))]
pub struct Registration {
    pub id: i64,
    pub user_id: i64,
    pub run_id: i64,
}
"#;
    let schema = schema_of(&[("registration.rs", src)]);
    let t = schema.table("registrations").unwrap();
    assert_eq!(
        t.composite_uniques,
        vec![vec!["user_id".to_string(), "run_id".to_string()]]
    );

    // Emitted as a CREATE UNIQUE INDEX at table-creation time, not an inline
    // table constraint (so it can be added/dropped without a rebuild).
    let m = diff_schemas(&Schema::default(), &schema).unwrap().unwrap();
    assert!(
        m.sql.contains(
            "CREATE UNIQUE INDEX \"idx_registrations_user_id_run_id\" ON \"registrations\" (\"user_id\", \"run_id\");"
        ),
        "got: {}",
        m.sql
    );
    let create = create_table_sql(t);
    assert!(
        !create.contains("UNIQUE (\"user_id\", \"run_id\")"),
        "should not be inline: {create}"
    );

    // Composite constraints survive a snapshot round-trip too.
    assert_eq!(schema_from_json(&schema_to_json(&schema)).unwrap(), schema);

    // Removing it later is a plain DROP INDEX, not a rebuild.
    let without = schema_of(&[(
        "registration.rs",
        &src.replace("#[orm(unique(user_id, run_id))]\n", ""),
    )]);
    let m = diff_schemas(&schema, &without).unwrap().unwrap();
    assert_eq!(
        m.sql,
        "DROP INDEX IF EXISTS \"idx_registrations_user_id_run_id\";\n"
    );
    let back = diff_schemas(&without, &schema).unwrap().unwrap();
    assert_eq!(
        back.sql,
        "CREATE UNIQUE INDEX \"idx_registrations_user_id_run_id\" ON \"registrations\" (\"user_id\", \"run_id\");\n"
    );
}

#[test]
fn struct_level_index_on_a_composite_primary_key_column() {
    // A single-column `#[orm(index)]` on a PK field is rejected as redundant
    // with the PK's own index — but for a *composite* PK, the PK's index is
    // ordered and doesn't help a lookup on a non-leading column alone, so the
    // struct-level `index(...)` form (not subject to that check) must still
    // be allowed here.
    let src = r#"
#[derive(Table)]
#[orm(index(user_id))]
pub struct EventManager {
    #[orm(primary_key, references(Event, on_delete = cascade))]
    pub event_id: i64,
    #[orm(primary_key)]
    pub user_id: i64,
}
"#;
    let schema = schema_of(&[("event.rs", EVENT), ("event_manager.rs", src)]);
    let t = schema.table("event_managers").unwrap();
    assert_eq!(t.composite_indexes, vec![vec!["user_id".to_string()]]);

    let m = diff_schemas(&Schema::default(), &schema).unwrap().unwrap();
    assert!(
        m.sql.contains(
            "CREATE INDEX \"idx_event_managers_user_id\" ON \"event_managers\" (\"user_id\");"
        ),
        "got: {}",
        m.sql
    );
}

#[test]
fn unique_referencing_unknown_column_is_rejected() {
    let src = r#"
#[derive(Table)]
#[orm(unique(user_id, nonexistent))]
pub struct Registration {
    pub id: i64,
    pub user_id: i64,
}
"#;
    let err = parse_sources(&[("r.rs".into(), src.into())]).unwrap_err();
    assert!(err.message.contains("nonexistent"), "got: {err}");
}

#[test]
fn snapshot_roundtrips() {
    let schema = v1();
    assert_eq!(schema_from_json(&schema_to_json(&schema)).unwrap(), schema);
}

#[test]
fn unsupported_types_and_missing_pk_are_rejected() {
    let no_pk = "#[derive(Table)]\npub struct Setting { pub key: String }";
    assert!(parse_sources(&[("s.rs".into(), no_pk.into())]).is_err());

    let bad_type =
        "#[derive(Table)]\npub struct Thing { pub id: i64, pub blob: HashMap<String, u8> }";
    let err = parse_sources(&[("t.rs".into(), bad_type.into())]).unwrap_err();
    assert!(err.message.contains("Thing.blob"), "got: {err}");

    // u64/usize/isize map to nothing SQLite can store (INTEGER is i64 and
    // sqlx-sqlite cannot encode them) — rejected with a pointed message, not
    // the generic "unsupported type" one.
    let big = "#[derive(Table)]\npub struct Thing { pub id: i64, pub big: u64 }";
    let err = parse_sources(&[("t.rs".into(), big.into())]).unwrap_err();
    assert!(err.message.contains("use i64"), "got: {err}");
}

#[test]
fn table_naming_convention() {
    assert_eq!(pluralize(&to_snake_case("Product")), "products");
    assert_eq!(pluralize(&to_snake_case("Category")), "categories");
    assert_eq!(pluralize(&to_snake_case("EventManager")), "event_managers");
    assert_eq!(pluralize(&to_snake_case("Box")), "boxes");

    let src = "#[derive(Table)]\n#[orm(table = \"people\")]\npub struct Person { pub id: i64 }";
    let schema = schema_of(&[("person.rs", src)]);
    assert_eq!(schema.tables[0].name, "people");
}

#[test]
fn model_attribute_marks_a_table() {
    // The framework's `#[model(...)]` attribute macro expands to
    // `#[derive(Table)]`, so the schema layer must pick such structs up from
    // source exactly like hand-derived tables.
    let src = r#"
#[model(permission = "content", api)]
pub struct Article {
    pub id: i64,
    #[orm(unique)]
    #[ui(list)]
    pub slug: String,
}
"#;
    let schema = schema_of(&[("article.rs", src)]);
    assert_eq!(schema.tables[0].name, "articles");
    assert!(schema.tables[0].column("slug").unwrap().unique);
}
