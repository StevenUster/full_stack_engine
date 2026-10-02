//! End-to-end tests for what a model can declare beyond plain CRUD:
//! relations (`show`, foreign-key selects and their visibility check),
//! nesting (`parent`), row actions, many-to-many links, declarative
//! validation (`required`, `email`, `url`, `min`/`max`, `slug_from`),
//! `format`, `order_by`/`per_page` and secret hiding — all over HTTP, with
//! templates that dump their context as JSON so every assertion sees exactly
//! what a page gets.

#![deny(warnings)]
#![allow(dead_code)]

use std::sync::Mutex;

use actix_web::cookie::Cookie;
use actix_web::http::header::LOCATION;
use actix_web::{App, test, web};
use full_stack_engine::auth::create_jwt;
use full_stack_engine::models::{self, ActionCx, AppResult, CurrentUser, Db, ModelHooks};
use full_stack_engine::prelude::{model, tera, update};
use full_stack_engine::structs::User;
use full_stack_engine::{AppData, Env, define_roles};
use serde_json::Value;
use sqlx::SqlitePool;

define_roles! {
    (Admin, "admin", ["all"]),
    (Coach, "coach", ["teams.read", "teams.write", "badges.read", "tasks.read", "tasks.write"]),
    (Helper, "helper", ["teams.read", "tasks.read", "tasks.write"]),
    (None, "none", ["none"]),
}

/// The ORM's relation code names a relation's target by module path —
/// `crate::{tables_dir}::{file}::{Struct}`, the one-struct-per-file layout of
/// an app's `src/models/`. This single-file test crate provides those paths
/// by re-export (`tables_dir = "tests"` in the framework's fse.toml).
mod tests {
    pub(crate) mod team {
        pub(crate) use crate::Team;
    }
    pub(crate) mod member {
        pub(crate) use crate::Member;
    }
}

/// A team belongs to its coach (`owner`); exercises the declarative field
/// rules and `format`.
#[model(path = "teams", owner = coach_id)]
struct Team {
    id: i64,
    coach_id: i64,
    #[ui(search)]
    name: String,
    #[orm(unique)]
    #[ui(slug_from = name)]
    slug: String,
    #[ui(url)]
    website: Option<String>,
    #[ui(email, required)]
    contact: Option<String>,
    #[orm(default = 5)]
    #[ui(min = 1, max = 50)]
    size: i64,
    #[ui(format = date)]
    founded: Option<String>,
    #[orm(default = 0.0)]
    #[ui(format = currency)]
    budget: f64,
    #[orm(default = false)]
    locked: bool,
}

/// Nested under its team: routes, scope and permissions come from the team.
#[model(parent = team_id, order_by = "name", per_page = 2, actions(promote), hooks)]
struct Member {
    id: i64,
    #[orm(references(Team, on_delete = cascade))]
    team_id: i64,
    #[orm(relation = team_id)]
    #[ui(show)]
    team: Option<Team>,
    name: String,
    #[orm(default = false)]
    captain: bool,
    api_token: Option<String>,
}

impl Member {
    async fn promote(&self, cx: ActionCx<'_>) -> AppResult<()> {
        let id = self.id;
        update!(Member, cx.db, id == id; captain = true).await?;
        Ok(())
    }
}

impl ModelHooks for Member {
    async fn can_create(db: &Db, _user: &CurrentUser, parent: Option<i64>) -> AppResult<bool> {
        let Some(team) = parent else { return Ok(false) };
        Ok(!Team::fetch(db, team).await?.is_some_and(|t| t.locked))
    }

    async fn can_act(&self, action: &str, _db: &Db, _user: &CurrentUser) -> AppResult<bool> {
        Ok(action == "promote" && !self.captain)
    }
}

/// Points at a member; its list shows the member and, one level deeper,
/// the member's team.
#[model(path = "tasks")]
struct Task {
    id: i64,
    title: String,
    #[orm(references(Member, on_delete = cascade))]
    member_id: i64,
    #[orm(relation = member_id)]
    #[ui(list, show(team))]
    member: Option<Member>,
}

#[model(path = "badges")]
struct Badge {
    id: i64,
    name: String,
}

/// Team ↔ badge, many-to-many; its hooks see every link change.
#[model(link = team_id, path = "badges", hooks)]
struct TeamBadge {
    #[orm(primary_key, references(Team, on_delete = cascade))]
    team_id: i64,
    #[orm(primary_key, references(Badge, on_delete = cascade))]
    badge_id: i64,
}

static LINK_LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

impl ModelHooks for TeamBadge {
    async fn after_save(&self, _db: &Db, _user: &CurrentUser, created: bool) -> AppResult<()> {
        LINK_LOG
            .lock()
            .unwrap()
            .push(format!("add:{}:{}:{created}", self.team_id, self.badge_id));
        Ok(())
    }

    async fn after_delete(self, _db: &Db, _user: &CurrentUser) -> AppResult<()> {
        LINK_LOG
            .lock()
            .unwrap()
            .push(format!("remove:{}:{}", self.team_id, self.badge_id));
        Ok(())
    }
}

/// Public pages + API; `chef_note` is for signed-in users only.
#[model(path = "recipe-admin", public_read = slug, api)]
struct Recipe {
    id: i64,
    #[ui(search)]
    name: String,
    #[orm(unique)]
    slug: String,
    #[ui(private, search, filter)]
    chef_note: Option<String>,
}

const SECRET: &str = "0123456789abcdef0123456789abcdef";

fn test_tera() -> tera::Tera {
    let mut t = tera::Tera::default();
    for name in [
        "fse/list",
        "fse/form",
        "fse/links",
        "fse/public-list",
        "fse/public-detail",
    ] {
        t.add_raw_template(name, "{{ __tera_context | safe }}")
            .unwrap();
    }
    t
}

fn app_data(db: SqlitePool) -> web::Data<AppData> {
    let mut config = full_stack_engine::testing::config(SECRET);
    config.currency = Some("EUR".into());
    web::Data::new(AppData {
        tera: test_tera(),
        db,
        env: Env::Prod,
        domain: String::new(),
        protocol: String::new(),
        config: std::sync::Arc::new(config),
        smtp_from: String::new(),
        email_verification_enabled: false,
        context_injector: None,
        locales: std::collections::HashMap::new(),
        locale_selector: full_stack_engine::i18n::LocaleSelector::Hardcoded("de".into()),
        themes: std::sync::Arc::default(),
    })
}

async fn login(db: &SqlitePool, name: &str, role: AppRole) -> (i64, Cookie<'static>) {
    let email = format!("features-{name}@test.dev");
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, password, sessions_valid_after) VALUES (?, 'x', 0) \
         ON CONFLICT(email) DO UPDATE SET password = 'x' RETURNING id",
    )
    .bind(&email)
    .fetch_one(db)
    .await
    .unwrap();
    let user = User::<AppRole> {
        id,
        email,
        password: String::new(),
        role,
        created_at: chrono::Utc::now().naive_utc(),
        is_verified: true,
        verification_token: None,
    };
    (id, Cookie::new("token", create_jwt(&user, SECRET).unwrap()))
}

macro_rules! call {
    ($app:expr, $req:expr) => {{
        let res = test::call_service(&$app, $req.to_request()).await;
        let status = res.status().as_u16();
        let location = res
            .headers()
            .get(LOCATION)
            .map(|v| v.to_str().unwrap().to_string());
        let body = String::from_utf8(test::read_body(res).await.to_vec()).unwrap();
        let json: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        (status, location, json)
    }};
}

fn id_from(location: Option<String>) -> i64 {
    location
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

fn codes(errors: &Value) -> Vec<String> {
    errors
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            format!(
                "{}:{}",
                e["field"].as_str().unwrap(),
                e["code"].as_str().unwrap()
            )
        })
        .collect()
}

#[actix_web::test]
async fn the_model_setup_is_consistent_and_routes_are_listed() {
    models::check().unwrap();
    let routes: Vec<String> = models::route_table()
        .iter()
        .filter(|r| r.model == "Member" || r.model == "TeamBadge")
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    assert!(
        routes.contains(&"GET /teams/{parent_id}/members".to_string()),
        "{routes:?}"
    );
    assert!(routes.contains(&"POST /teams/{parent_id}/members/{id}/actions/{action}".to_string()));
    assert!(routes.contains(&"POST /teams/{id}/badges".to_string()));
    assert!(routes.contains(&"DELETE /teams/{id}/badges/{other_id}".to_string()));
    // A nested model inherits its parent's permission base.
    let members = models::model("members").unwrap();
    assert_eq!(members.read_permission(), "teams.read");
    // Secret-looking columns are hidden and never a form field.
    assert!(members.ui_field("api_token").unwrap().hidden);
    assert!(!members.ui.form_fields.contains(&"api_token"));
}

/// One test: the suite shares the build.rs database file.
#[actix_web::test]
#[allow(clippy::too_many_lines)]
async fn model_features_over_http() {
    let db = SqlitePool::connect(env!("DATABASE_URL")).await.unwrap();
    for table in [
        "tasks",
        "members",
        "team_badges",
        "badges",
        "teams",
        "recipes",
    ] {
        sqlx::query(&format!("DELETE FROM {table}"))
            .execute(&db)
            .await
            .unwrap();
    }
    let (_, admin) = login(&db, "admin", AppRole::Admin).await;
    let (_, coach) = login(&db, "coach", AppRole::Coach).await;
    let (_, rival) = login(&db, "rival", AppRole::Coach).await;
    let (_, helper) = login(&db, "helper", AppRole::Helper).await;

    let app = test::init_service(
        App::new()
            .app_data(app_data(db.clone()))
            .configure(models::mount_all::<AppRole>),
    )
    .await;

    // ---- declarative validation: every rule reports at once.
    let (status, _, ctx) = call!(
        app,
        test::TestRequest::post()
            .uri("/teams/create")
            .cookie(coach.clone())
            .set_form([
                ("name", "Falcons"),
                ("website", "javascript:alert(1)"),
                ("contact", ""),
                ("size", "99"),
            ])
    );
    assert_eq!(status, 200);
    let errs = codes(&ctx["errors"]);
    for expected in ["website:invalid_url", "contact:required", "size:too_large"] {
        assert!(errs.contains(&expected.to_string()), "{errs:?}");
    }
    let (_, _, ctx) = call!(
        app,
        test::TestRequest::post()
            .uri("/teams/create")
            .cookie(coach.clone())
            .set_form([
                ("name", "Falcons"),
                ("contact", "not-an-email"),
                ("size", "0")
            ])
    );
    let errs = codes(&ctx["errors"]);
    assert!(
        errs.contains(&"contact:invalid_email".to_string()),
        "{errs:?}"
    );
    assert!(errs.contains(&"size:too_small".to_string()), "{errs:?}");

    // ---- slug_from + owner: the slug is derived, the coach owns the team.
    let (status, location, _) = call!(
        app,
        test::TestRequest::post()
            .uri("/teams/create")
            .cookie(coach.clone())
            .set_form([
                ("name", "Falcons United"),
                ("website", "https://falcons.test"),
                ("contact", "coach@falcons.test"),
                ("size", "11"),
                ("founded", "2020-05-17"),
                ("budget", "1234.5"),
            ])
    );
    assert_eq!(status, 302);
    let falcons = id_from(location);
    let (_, location, _) = call!(
        app,
        test::TestRequest::post()
            .uri("/teams/create")
            .cookie(rival.clone())
            .set_form([("name", "Rivals"), ("contact", "r@rivals.test")])
    );
    let rivals = id_from(location);

    // ---- format: display values in the request language (de).
    let (_, _, ctx) = call!(
        app,
        test::TestRequest::get()
            .uri(&format!("/teams/{falcons}"))
            .cookie(coach.clone())
    );
    assert_eq!(ctx["row"]["slug"], "falcons-united");
    assert_eq!(ctx["row"]["founded_display"], "17.05.2020");
    assert_eq!(ctx["row"]["budget_display"], "1.234,50\u{a0}€");

    // ---- nested: members live under a team the user can see.
    for name in ["Zoe", "Adam", "Mia"] {
        let (status, location, _) = call!(
            app,
            test::TestRequest::post()
                .uri(&format!("/teams/{falcons}/members/create"))
                .cookie(coach.clone())
                .set_form([("name", name), ("team_id", &rivals.to_string())])
        );
        assert_eq!(status, 302, "{name}");
        assert!(
            location
                .unwrap()
                .starts_with(&format!("/teams/{falcons}/members/")),
            "redirects stay under the parent"
        );
    }
    // A posted team_id is ignored: the parent comes from the URL.
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM members WHERE team_id = ?")
            .bind(falcons)
            .fetch_one(&db)
            .await
            .unwrap(),
        3
    );
    // The list: per_page = 2, order_by = name, the parent in the context,
    // relations embedded, secrets absent.
    let (status, _, ctx) = call!(
        app,
        test::TestRequest::get()
            .uri(&format!("/teams/{falcons}/members"))
            .cookie(coach.clone())
    );
    assert_eq!(status, 200);
    assert_eq!(ctx["total"], 3);
    let names: Vec<&str> = ctx["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Adam", "Mia"]);
    assert_eq!(ctx["parent"]["row"]["name"], "Falcons United");
    assert_eq!(
        ctx["meta"]["base_path"],
        format!("/teams/{falcons}/members")
    );
    assert_eq!(ctx["rows"][0]["team"]["title"], "Falcons United");
    assert!(
        ctx["rows"][0].get("api_token").is_none(),
        "secrets never render"
    );
    assert_eq!(ctx["can_create"], true);
    assert_eq!(ctx["rows"][0]["_actions"], serde_json::json!(["promote"]));
    let adam = ctx["rows"][0]["id"].as_i64().unwrap();
    // Another coach can't see the parent — so nothing under it.
    for uri in [
        format!("/teams/{falcons}/members"),
        format!("/teams/{falcons}/members/{adam}"),
    ] {
        let (status, _, _) = call!(
            app,
            test::TestRequest::get().uri(&uri).cookie(rival.clone())
        );
        assert_eq!(status, 404, "{uri}");
    }
    // A member is only reachable under its own team.
    let (status, _, _) = call!(
        app,
        test::TestRequest::get()
            .uri(&format!("/teams/{rivals}/members/{adam}"))
            .cookie(admin.clone())
    );
    assert_eq!(status, 404);
    // Sorting by a hidden column is ignored (no ordering side channel).
    let (_, _, ctx) = call!(
        app,
        test::TestRequest::get()
            .uri(&format!("/teams/{falcons}/members?sort=api_token&dir=desc"))
            .cookie(coach.clone())
    );
    assert_eq!(ctx["rows"][0]["name"], "Adam");

    // ---- actions: run, then refused once can_act says no.
    let (status, location, _) = call!(
        app,
        test::TestRequest::post()
            .uri(&format!("/teams/{falcons}/members/{adam}/actions/promote"))
            .cookie(coach.clone())
    );
    assert_eq!(status, 303);
    assert_eq!(
        location.unwrap(),
        format!("/teams/{falcons}/members/{adam}")
    );
    let (_, _, ctx) = call!(
        app,
        test::TestRequest::get()
            .uri(&format!("/teams/{falcons}/members/{adam}"))
            .cookie(coach.clone())
    );
    assert_eq!(ctx["row"]["captain"], true);
    assert_eq!(ctx["actions"], serde_json::json!([]));
    let (status, _, _) = call!(
        app,
        test::TestRequest::post()
            .uri(&format!("/teams/{falcons}/members/{adam}/actions/promote"))
            .cookie(coach.clone())
    );
    assert_eq!(status, 401);
    let (status, _, _) = call!(
        app,
        test::TestRequest::post()
            .uri(&format!("/teams/{falcons}/members/{adam}/actions/nope"))
            .cookie(coach.clone())
    );
    assert_eq!(status, 404);

    // ---- can_create with the parent: a locked team takes no members.
    sqlx::query("UPDATE teams SET locked = 1 WHERE id = ?")
        .bind(falcons)
        .execute(&db)
        .await
        .unwrap();
    let (_, _, ctx) = call!(
        app,
        test::TestRequest::get()
            .uri(&format!("/teams/{falcons}/members"))
            .cookie(coach.clone())
    );
    assert_eq!(ctx["can_create"], false);
    let (status, _, _) = call!(
        app,
        test::TestRequest::post()
            .uri(&format!("/teams/{falcons}/members/create"))
            .cookie(coach.clone())
            .set_form([("name", "Late")])
    );
    assert_eq!(status, 401);

    // ---- relations: a select of what the user may read, deep embedding,
    // and foreign keys checked against visibility.
    let (_, _, ctx) = call!(
        app,
        test::TestRequest::get()
            .uri("/tasks/create")
            .cookie(coach.clone())
    );
    let member_col = ctx["meta"]["form_columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "member_id")
        .unwrap()
        .clone();
    assert_eq!(member_col["widget"], "relation");
    assert_eq!(member_col["options"].as_array().unwrap().len(), 3);
    assert_eq!(member_col["options"][0]["title"], "Adam");

    let (status, location, _) = call!(
        app,
        test::TestRequest::post()
            .uri("/tasks/create")
            .cookie(coach.clone())
            .set_form([("title", "Warm up"), ("member_id", &adam.to_string())])
    );
    assert_eq!(status, 302);
    let task = id_from(location);
    let (_, _, ctx) = call!(
        app,
        test::TestRequest::get().uri("/tasks").cookie(coach.clone())
    );
    let row = &ctx["rows"][0];
    assert_eq!(row["member"]["title"], "Adam");
    assert_eq!(row["member"]["team"]["title"], "Falcons United");
    assert!(
        ctx["meta"]["list_columns"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "member_id" && c["relation"] == "member")
    );
    // The rival coach can't attach a task to a member they can't see.
    let (status, _, ctx) = call!(
        app,
        test::TestRequest::post()
            .uri("/tasks/create")
            .cookie(rival.clone())
            .set_form([("title", "Sabotage"), ("member_id", &adam.to_string())])
    );
    assert_eq!(status, 200);
    assert_eq!(codes(&ctx["errors"]), ["member_id:invalid_option"]);
    // ...nor move an existing one there; an unchanged value is fine.
    let (status, _, _) = call!(
        app,
        test::TestRequest::post()
            .uri(&format!("/tasks/{task}"))
            .cookie(coach.clone())
            .set_form([("title", "Warm up!"), ("member_id", &adam.to_string())])
    );
    assert_eq!(status, 302);

    // ---- private columns: signed-in only, invisible and unusable publicly.
    let (_, _, _) = call!(
        app,
        test::TestRequest::post()
            .uri("/recipe-admin/create")
            .cookie(admin.clone())
            .set_form([
                ("name", "Soup"),
                ("slug", "soup"),
                ("chef_note", "extra garlic")
            ])
    );
    let (_, _, ctx) = call!(app, test::TestRequest::get().uri("/recipes"));
    assert_eq!(ctx["rows"][0]["name"], "Soup");
    assert!(
        ctx["rows"][0].get("chef_note").is_none(),
        "private on a public page"
    );
    assert!(
        ctx["meta"].get("form_columns").is_none(),
        "admin metadata on a public page"
    );
    for query in ["search=garlic", "chef_note=garlic"] {
        let (_, _, ctx) = call!(
            app,
            test::TestRequest::get().uri(&format!("/recipes?{query}"))
        );
        // The private match is ignored: the search finds nothing, the
        // filter doesn't narrow — neither reveals the note.
        let expected = if query.starts_with("search") { 0 } else { 1 };
        assert_eq!(ctx["total"], expected, "{query}");
    }
    let (_, _, json) = call!(app, test::TestRequest::get().uri("/api/recipes/soup"));
    assert_eq!(json["name"], "Soup");
    assert!(json.get("chef_note").is_none());
    let (_, _, ctx) = call!(
        app,
        test::TestRequest::get()
            .uri("/recipe-admin?search=garlic")
            .cookie(admin.clone())
    );
    assert_eq!(
        ctx["total"], 1,
        "signed in, the private column is searchable"
    );

    // ---- links: page, candidates, add, remove — hooks see each change.
    let (_, location, _) = call!(
        app,
        test::TestRequest::post()
            .uri("/badges/create")
            .cookie(admin.clone())
            .set_form([("name", "Fair Play")])
    );
    let badge = id_from(location);
    let (status, _, ctx) = call!(
        app,
        test::TestRequest::get()
            .uri(&format!("/teams/{falcons}/badges/candidates?search=fair"))
            .cookie(coach.clone())
    );
    assert_eq!(status, 200);
    assert_eq!(ctx["rows"][0]["title"], "Fair Play");
    assert_eq!(ctx["rows"][0]["linked"], false);
    let (status, location, _) = call!(
        app,
        test::TestRequest::post()
            .uri(&format!("/teams/{falcons}/badges"))
            .cookie(coach.clone())
            .set_form([("id", badge.to_string())])
    );
    assert_eq!(status, 303);
    assert_eq!(location.unwrap(), format!("/teams/{falcons}/badges"));
    let (_, _, ctx) = call!(
        app,
        test::TestRequest::get()
            .uri(&format!("/teams/{falcons}/badges"))
            .cookie(coach.clone())
    );
    assert_eq!(ctx["linked"][0]["title"], "Fair Play");
    assert_eq!(ctx["parent"]["row"]["name"], "Falcons United");
    // Without read permission on the linked model, no link pages at all.
    let (status, _, _) = call!(
        app,
        test::TestRequest::get()
            .uri(&format!("/teams/{falcons}/badges"))
            .cookie(helper.clone())
    );
    assert_eq!(status, 401);
    // The rival can't touch another coach's team's links.
    let (status, _, _) = call!(
        app,
        test::TestRequest::delete()
            .uri(&format!("/teams/{falcons}/badges/{badge}"))
            .cookie(rival.clone())
    );
    assert_eq!(status, 404);
    let (status, _, _) = call!(
        app,
        test::TestRequest::delete()
            .uri(&format!("/teams/{falcons}/badges/{badge}"))
            .cookie(coach.clone())
    );
    assert_eq!(status, 200);

    // ---- delete from a plain HTML form: POST .../delete, then `_back`.
    let (status, location, _) = call!(
        app,
        test::TestRequest::post()
            .uri(&format!("/tasks/{task}/delete"))
            .cookie(coach.clone())
            .set_form([("_back", "https://evil.test")])
    );
    assert_eq!(status, 303);
    assert_eq!(location.unwrap(), "/tasks", "an outside _back is ignored");
    assert_eq!(
        *LINK_LOG.lock().unwrap(),
        [
            format!("add:{falcons}:{badge}:true"),
            format!("remove:{falcons}:{badge}")
        ]
    );
}
