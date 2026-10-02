//! End-to-end tests for app logic on the model: `ModelHooks` and the
//! `owner` shortcut, driven over HTTP through the generated routes.
//!
//! The shapes are taken from a real app (running-for-jesus): a campaign is
//! visible to admins and to the managers linked through a join table, is
//! locked once completed, and only admins may create or delete one; a
//! pledge belongs to the donor who made it. Before phase 9 that app had to
//! hand-write all of these handlers.

#![deny(warnings)]
#![allow(dead_code)]

use std::sync::Mutex;

use actix_web::cookie::Cookie;
use actix_web::http::header::LOCATION;
use actix_web::{App, test, web};
use chrono::NaiveDateTime;
use full_stack_engine::auth::create_jwt;
use full_stack_engine::models::{
    self, AppResult, Cond, CurrentUser, Db, FieldError, FormData, FormErrors, ModelHooks, SaveCx,
};
use full_stack_engine::prelude::{Table, model, tera};
use full_stack_engine::structs::User;
use full_stack_engine::{AppData, Env, define_roles};
use sqlx::SqlitePool;

define_roles! {
    (Admin, "admin", ["all"]),
    (Manager, "manager", ["campaigns.read", "campaigns.write"]),
    (Donor, "donor", ["pledges.read", "pledges.write"]),
    (None, "none", ["none"]),
}

#[model(path = "campaign-manager", permission = "campaigns", public_read = slug, hooks)]
struct Campaign {
    id: i64,
    #[ui(list, search)]
    name: String,
    #[orm(unique)]
    slug: String,
    #[ui(filter)]
    location: Option<String>,
    /// ISO `YYYY-MM-DD` kept as text — ranges compare lexicographically.
    #[ui(filter = range)]
    date: String,
    #[orm(default = false)]
    #[ui(readonly)]
    is_completed: bool,
    #[orm(default = now)]
    #[ui(filter)]
    created_at: NaiveDateTime,
}

/// Which managers run which campaign — composite key, so a plain table.
#[derive(Table, Debug, Clone)]
struct CampaignManager {
    #[orm(primary_key)]
    campaign_id: i64,
    #[orm(primary_key)]
    user_id: i64,
}

/// What the delete hooks saw, in order, to prove both ran with the row.
static DELETED: Mutex<Vec<String>> = Mutex::new(Vec::new());

impl ModelHooks for Campaign {
    async fn scope(db: &Db, user: &CurrentUser) -> AppResult<Option<Cond>> {
        if user.is_admin() {
            return Ok(None);
        }
        let ids: Vec<i64> = CampaignManager::find()
            .filter(CampaignManager::USER_ID.eq(user.id()))
            .fetch_all(db)
            .await?
            .into_iter()
            .map(|m| m.campaign_id)
            .collect();
        Ok(Some(Campaign::ID.in_(ids)))
    }

    fn public_scope() -> Option<Cond> {
        Some(Campaign::IS_COMPLETED.eq(false))
    }

    async fn can_create(_db: &Db, user: &CurrentUser, _parent: Option<i64>) -> AppResult<bool> {
        Ok(user.role::<AppRole>() == AppRole::Admin)
    }

    async fn can_edit(&self, _db: &Db, _user: &CurrentUser) -> AppResult<bool> {
        Ok(!self.is_completed)
    }

    async fn can_delete(&self, _db: &Db, user: &CurrentUser) -> AppResult<bool> {
        Ok(user.is_admin())
    }

    async fn before_save(form: &mut FormData, cx: SaveCx<'_, Self>) -> AppResult<FormErrors> {
        let name = form.get("name").cloned().unwrap_or_default();
        if form.get("slug").is_none_or(|s| s.trim().is_empty()) {
            // Keep the stored slug on edit (stable URLs); derive it on create.
            let slug = cx.existing.map_or_else(
                || full_stack_engine::text::slugify(&name),
                |e| e.slug.clone(),
            );
            form.insert("slug".into(), slug);
        }
        let mut errors = Vec::new();
        if name.contains("http") {
            errors.push(FieldError {
                field: "name",
                code: "no_links",
            });
        }
        Ok(errors)
    }

    async fn before_delete(&self, db: &Db, _user: &CurrentUser) -> AppResult<()> {
        // The row still exists here — cleanup can read it and its children.
        let still_there = Campaign::fetch(db, self.id).await?.is_some();
        DELETED
            .lock()
            .unwrap()
            .push(format!("before:{still_there}"));
        Ok(())
    }

    async fn after_delete(self, db: &Db, _user: &CurrentUser) -> AppResult<()> {
        let still_there = Campaign::fetch(db, self.id).await?.is_some();
        DELETED
            .lock()
            .unwrap()
            .push(format!("after:{}:{still_there}", self.slug));
        Ok(())
    }

    fn decorate(&self, row: &mut serde_json::Map<String, serde_json::Value>) {
        row.insert(
            "label".into(),
            format!("{} ({})", self.name, self.date).into(),
        );
    }
}

#[model(path = "my-pledges", permission = "pledges", owner = donor_id)]
struct Pledge {
    id: i64,
    donor_id: i64,
    #[ui(list)]
    amount: f64,
    note: Option<String>,
}

const SECRET: &str = "0123456789abcdef0123456789abcdef";

fn test_tera() -> tera::Tera {
    let mut t = tera::Tera::default();
    t.autoescape_on(vec![""]);
    t.add_raw_template(
        "fse/list",
        "LIST total={{ total }} create={{ can_create }} \
         rows={% for r in rows %}{{ r.slug | default(value=r.id) }},{% endfor %}",
    )
    .unwrap();
    t.add_raw_template(
        "fse/form",
        "FORM edit={{ can_edit }} delete={{ can_delete }} \
         errors={% for e in errors %}{{ e.field }}:{{ e.code }},{% endfor %} \
         slug={{ row.slug | default(value='') }} label={{ row.label | default(value='') }}",
    )
    .unwrap();
    t.add_raw_template(
        "fse/public-list",
        "PUB rows={% for r in rows %}{{ r.slug }},{% endfor %}",
    )
    .unwrap();
    t.add_raw_template("fse/public-detail", "PUBDET {{ row.slug }}")
        .unwrap();
    t
}

fn app_data(db: SqlitePool) -> web::Data<AppData> {
    web::Data::new(AppData {
        tera: test_tera(),
        db,
        env: Env::Prod,
        domain: String::new(),
        protocol: String::new(),
        config: std::sync::Arc::new(full_stack_engine::testing::config(SECRET)),
        smtp_from: String::new(),
        email_verification_enabled: false,
        context_injector: None,
        locales: std::collections::HashMap::new(),
        locale_selector: full_stack_engine::i18n::LocaleSelector::default(),
        themes: std::sync::Arc::default(),
    })
}

/// A signed-in user of `role` (one per name), as `(id, cookie)`.
async fn login(db: &SqlitePool, name: &str, role: AppRole) -> (i64, Cookie<'static>) {
    let email = format!("hooks-{name}@test.dev");
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
        (status, location, body)
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

/// One test: the suite shares the build.rs database file.
#[actix_web::test]
async fn hooks_and_owner_over_http() {
    let db = SqlitePool::connect(env!("DATABASE_URL")).await.unwrap();
    Campaign::delete_where().execute(&db).await.unwrap();
    CampaignManager::delete_where().execute(&db).await.unwrap();
    Pledge::delete_where().execute(&db).await.unwrap();

    let (_, admin) = login(&db, "admin", AppRole::Admin).await;
    let (manager_id, manager) = login(&db, "manager", AppRole::Manager).await;
    let (donor_id, donor) = login(&db, "donor", AppRole::Donor).await;
    let (_, other_donor) = login(&db, "donor2", AppRole::Donor).await;

    let app = test::init_service(
        App::new()
            .app_data(app_data(db.clone()))
            .configure(models::mount_all::<AppRole>),
    )
    .await;

    // ---- can_create: the role may write, but only admins create.
    let (status, _, _) = call!(
        app,
        test::TestRequest::get()
            .uri("/campaign-manager/create")
            .cookie(manager.clone())
    );
    assert_eq!(status, 401);
    let (status, _, _) = call!(
        app,
        test::TestRequest::post()
            .uri("/campaign-manager/create")
            .cookie(manager.clone())
            .set_form([("name", "Sneaky"), ("date", "2026-01-01")])
    );
    assert_eq!(status, 401);

    // ---- before_save fills the slug from the name.
    let (status, location, _) = call!(
        app,
        test::TestRequest::post()
            .uri("/campaign-manager/create")
            .cookie(admin.clone())
            .set_form([
                ("name", "Spring Run"),
                ("slug", ""),
                ("location", "Berlin Mitte"),
                ("date", "2026-04-10"),
            ])
    );
    assert_eq!(status, 302);
    let spring = id_from(location);
    let (_, location, _) = call!(
        app,
        test::TestRequest::post()
            .uri("/campaign-manager/create")
            .cookie(admin.clone())
            .set_form([
                ("name", "Autumn Walk"),
                ("location", "Hamburg"),
                ("date", "2026-10-03"),
            ])
    );
    let autumn = id_from(location);

    // ...and its errors re-render the form next to the parse errors.
    let (status, _, body) = call!(
        app,
        test::TestRequest::post()
            .uri("/campaign-manager/create")
            .cookie(admin.clone())
            .set_form([("name", "see http://spam"), ("date", "")])
    );
    assert_eq!(status, 200);
    assert!(body.contains("name:no_links,"), "{body}");
    assert!(body.contains("date:required,"), "{body}");

    // ---- scope: the manager runs Spring Run only.
    sqlx::query("INSERT INTO campaign_managers (campaign_id, user_id) VALUES (?, ?)")
        .bind(spring)
        .bind(manager_id)
        .execute(&db)
        .await
        .unwrap();

    let (_, _, body) = call!(
        app,
        test::TestRequest::get()
            .uri("/campaign-manager")
            .cookie(manager.clone())
    );
    assert_eq!(body, "LIST total=1 create=false rows=spring-run,");
    let (_, _, body) = call!(
        app,
        test::TestRequest::get()
            .uri("/campaign-manager?sort=name")
            .cookie(admin.clone())
    );
    assert_eq!(
        body,
        "LIST total=2 create=true rows=autumn-walk,spring-run,"
    );

    // Outside the scope is a 404 for every endpoint, never a 401 that
    // would confirm the row exists.
    for req in [
        test::TestRequest::get().uri(&format!("/campaign-manager/{autumn}")),
        test::TestRequest::post()
            .uri(&format!("/campaign-manager/{autumn}"))
            .set_form([("name", "Hijacked"), ("date", "2026-10-03")]),
        test::TestRequest::delete().uri(&format!("/campaign-manager/{autumn}")),
    ] {
        let (status, _, _) = call!(app, req.cookie(manager.clone()));
        assert_eq!(status, 404);
    }

    // ---- inside the scope: editable, not deletable (admins only);
    // decorate's computed field reaches the page.
    let (_, _, body) = call!(
        app,
        test::TestRequest::get()
            .uri(&format!("/campaign-manager/{spring}"))
            .cookie(manager.clone())
    );
    assert!(body.contains("edit=true delete=false"), "{body}");
    assert!(body.contains("label=Spring Run (2026-04-10)"), "{body}");
    let (status, _, _) = call!(
        app,
        test::TestRequest::delete()
            .uri(&format!("/campaign-manager/{spring}"))
            .cookie(manager.clone())
    );
    assert_eq!(status, 401);

    // An empty slug on edit keeps the stored one (cx.existing).
    let (status, _, _) = call!(
        app,
        test::TestRequest::post()
            .uri(&format!("/campaign-manager/{spring}"))
            .cookie(manager.clone())
            .set_form([
                ("name", "Spring Run 2026"),
                ("slug", ""),
                ("location", "Berlin Mitte"),
                ("date", "2026-04-11"),
            ])
    );
    assert_eq!(status, 302);
    let stored = Campaign::fetch(&db, spring).await.unwrap().unwrap();
    assert_eq!(stored.slug, "spring-run");
    assert_eq!(stored.name, "Spring Run 2026");

    // ---- filters: contains, a text range, a timestamp range.
    for (query, expected) in [
        ("location=mitte", "rows=spring-run,"),
        ("date_from=2026-05-01", "rows=autumn-walk,"),
        ("date_to=2026-04-30", "rows=spring-run,"),
        (
            "date_from=2026-01-01&date_to=2026-12-31",
            "rows=autumn-walk,spring-run,",
        ),
        ("created_at_to=2000-01-01", "rows="),
        (
            "created_at_from=2000-01-01&location=hamburg",
            "rows=autumn-walk,",
        ),
        // Unparsable bounds are ignored, like unknown filters.
        ("created_at_from=yesterday", "rows=autumn-walk,spring-run,"),
    ] {
        let (_, _, body) = call!(
            app,
            test::TestRequest::get()
                .uri(&format!("/campaign-manager?sort=name&{query}"))
                .cookie(admin.clone())
        );
        assert!(body.ends_with(expected), "{query}: {body}");
    }

    // ---- can_edit: a completed campaign is read-only, even for admins.
    sqlx::query("UPDATE campaigns SET is_completed = 1 WHERE id = ?")
        .bind(spring)
        .execute(&db)
        .await
        .unwrap();
    let (_, _, body) = call!(
        app,
        test::TestRequest::get()
            .uri(&format!("/campaign-manager/{spring}"))
            .cookie(admin.clone())
    );
    assert!(body.contains("edit=false delete=true"), "{body}");
    let (status, _, _) = call!(
        app,
        test::TestRequest::post()
            .uri(&format!("/campaign-manager/{spring}"))
            .cookie(manager.clone())
            .set_form([("name", "Late edit"), ("date", "2026-04-11")])
    );
    assert_eq!(status, 401);

    // ---- public_scope: completed campaigns are off the public pages.
    let (_, _, body) = call!(app, test::TestRequest::get().uri("/campaigns"));
    assert_eq!(body, "PUB rows=autumn-walk,");
    let (status, _, _) = call!(app, test::TestRequest::get().uri("/campaigns/spring-run"));
    assert_eq!(status, 404);
    let (_, _, body) = call!(app, test::TestRequest::get().uri("/campaigns/autumn-walk"));
    assert_eq!(body, "PUBDET autumn-walk");

    // ---- before_delete runs with the row still stored, after_delete with
    // it gone.
    let (status, _, _) = call!(
        app,
        test::TestRequest::delete()
            .uri(&format!("/campaign-manager/{autumn}"))
            .cookie(admin.clone())
    );
    assert_eq!(status, 200);
    assert_eq!(
        *DELETED.lock().unwrap(),
        ["before:true", "after:autumn-walk:false"]
    );

    // ---- owner: filled from the user, a posted value is ignored.
    let (status, location, _) = call!(
        app,
        test::TestRequest::post()
            .uri("/my-pledges/create")
            .cookie(donor.clone())
            .set_form([("amount", "25"), ("donor_id", "999999")])
    );
    assert_eq!(status, 302);
    let pledge = id_from(location);
    assert_eq!(
        Pledge::fetch(&db, pledge).await.unwrap().unwrap().donor_id,
        donor_id
    );
    let m = models::model("pledges").unwrap();
    assert_eq!(m.ui.owner, Some("donor_id"));
    assert_eq!(m.ui.form_fields, ["amount", "note"]);

    let (_, _, body) = call!(
        app,
        test::TestRequest::get()
            .uri("/my-pledges")
            .cookie(donor.clone())
    );
    assert!(body.starts_with("LIST total=1 "), "{body}");
    let (_, _, body) = call!(
        app,
        test::TestRequest::get()
            .uri("/my-pledges")
            .cookie(other_donor.clone())
    );
    assert!(body.starts_with("LIST total=0 "), "{body}");
    let (status, _, _) = call!(
        app,
        test::TestRequest::post()
            .uri(&format!("/my-pledges/{pledge}"))
            .cookie(other_donor.clone())
            .set_form([("amount", "1")])
    );
    assert_eq!(status, 404);
    // Admins see every row.
    let (_, _, body) = call!(
        app,
        test::TestRequest::get()
            .uri("/my-pledges")
            .cookie(admin.clone())
    );
    assert!(body.starts_with("LIST total=1 "), "{body}");
}
