//! The module system's runtime contracts: module routes mount between the
//! app's (app wins) and the generated CRUD, and module locales layer between
//! the framework's and the app's.

use actix_web::{App, HttpResponse, test, web};
use full_stack_engine::i18n::{build_locales, resolve_locales};
use full_stack_engine::modules::ModuleDef;
use include_dir::{Dir, include_dir};

static APP_LOCALES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/tests/fixtures/locales");
static MODULE_LOCALES: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/tests/fixtures/module_locales");

fn shop_module() -> ModuleDef {
    ModuleDef::new("shop")
        .routes(|cfg| {
            cfg.route(
                "/shop",
                web::get().to(|| async { HttpResponse::Ok().body("MODULE SHOP") }),
            );
            cfg.route(
                "/clash",
                web::get().to(|| async { HttpResponse::Ok().body("MODULE") }),
            );
        })
        .locales(&MODULE_LOCALES)
}

/// Same registration order as `FrameworkApp::run`: app `configure` first,
/// then module routes — so the app's `/clash` shadows the module's.
#[actix_web::test]
async fn app_routes_shadow_module_routes() {
    let module = shop_module();
    let app = test::init_service(
        App::new()
            .configure(|cfg| {
                cfg.route(
                    "/clash",
                    web::get().to(|| async { HttpResponse::Ok().body("APP") }),
                );
            })
            .configure(module.routes.expect("shop module has routes")),
    )
    .await;

    let res = test::call_service(&app, test::TestRequest::get().uri("/shop").to_request()).await;
    let body = String::from_utf8(test::read_body(res).await.to_vec()).unwrap();
    assert_eq!(body, "MODULE SHOP");

    let res = test::call_service(&app, test::TestRequest::get().uri("/clash").to_request()).await;
    let body = String::from_utf8(test::read_body(res).await.to_vec()).unwrap();
    assert_eq!(body, "APP");
}

#[actix_web::test]
async fn module_locales_layer_between_framework_and_app() {
    let module = shop_module();
    let locales = resolve_locales(
        build_locales(&[module.locales.expect("shop module has locales")], Some(&APP_LOCALES)),
        "en",
    );

    let en = &locales["en"];
    // framework "Save" < module "Module Save" < app "Store": app wins.
    assert_eq!(en["model_ui"]["save"], "Store");
    // Module-only keys survive the app overlay.
    assert_eq!(en["shop"]["title"], "Shop");
    // Framework keys nobody overrode are still there.
    assert_eq!(en["model_ui"]["cancel"], "Cancel");

    let de = &locales["de"];
    // Module's German + framework's German + per-key fallback to en.
    assert_eq!(de["shop"]["title"], "Laden");
    assert_eq!(de["model_ui"]["save"], "Speichern");
    assert_eq!(de["app_name"], "Probe App");
}
