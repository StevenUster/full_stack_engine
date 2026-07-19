//! The `/uploads` static mount must serve every file with a `sandbox` CSP, so
//! an uploaded document that can carry script (an app may allow `.svg`/`.html`
//! in `save_upload`) can't execute in the site's origin when opened directly
//! — that would be stored XSS, since the site-wide CSP allows inline scripts.
//!
//! Own integration-test binary (= own process): it changes the working
//! directory, which would race other tests in a shared process.

use actix_web::{App, http::StatusCode, test};
use std::io::Write;

#[actix_web::test]
async fn uploads_are_served_with_a_sandbox_csp() {
    let workdir = tempfile::tempdir().unwrap();
    std::env::set_current_dir(workdir.path()).unwrap();

    // A hostile "image": actually an HTML document with an inline script.
    std::fs::create_dir_all("uploads/avatars").unwrap();
    let mut f = std::fs::File::create("uploads/avatars/evil.html").unwrap();
    f.write_all(b"<script>alert(document.cookie)</script>")
        .unwrap();

    let app = test::init_service(App::new().service(full_stack_engine::uploads_service())).await;

    let req = test::TestRequest::get()
        .uri("/uploads/avatars/evil.html")
        .to_request();
    let res = test::call_service(&app, req).await;

    assert_eq!(res.status(), StatusCode::OK, "the file is still served");
    let csp = res
        .headers()
        .get("Content-Security-Policy")
        .expect("uploads must carry a CSP")
        .to_str()
        .unwrap();
    assert!(
        csp.contains("sandbox"),
        "uploaded documents must be sandboxed so inline script can't run in the site origin, got: {csp}"
    );
}
