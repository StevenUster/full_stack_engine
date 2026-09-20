//! Send a test email using a real email template rendered with fake data + real translations.
//!
//! Configure the two constants below, then run:
//!   cargo run --bin test_email
//!
//! Prerequisites:
//!   - the theme is built (`bun run build` inside theme/) — the email renders
//!     from the same theme stack the app uses (child over fse-theme-default)
//!   - SMTP_* env vars set in .env

// ── Configure here ──────────────────────────────────────────────────────────
const TO_EMAIL: &str = "you@example.com";
const TEMPLATE: Template = Template::Verify;
// ────────────────────────────────────────────────────────────────────────────

use full_stack_engine::mail::send_mail;
use full_stack_engine::prelude::{serde_json, tera};

#[allow(dead_code)]
#[derive(Clone, Copy)]
enum Template {
    Verify,
    VerifyEmailChange,
    PasswordReset,
}

impl Template {
    fn path(self) -> &'static str {
        match self {
            Self::Verify => "emails/verify",
            Self::VerifyEmailChange => "emails/verify-email-change",
            Self::PasswordReset => "emails/password-reset",
        }
    }

    fn subject(self, t: &serde_json::Value) -> String {
        let val = match self {
            Self::Verify => &t["verify_email"]["subject"],
            Self::VerifyEmailChange => &t["verify_email_change"]["subject"],
            Self::PasswordReset => &t["password_reset_email"]["subject"],
        };
        val.as_str().unwrap_or("Test Email").to_string()
    }

    fn context(self, t: serde_json::Value) -> serde_json::Value {
        let base_url = "https://example.com";
        match self {
            Self::Verify => serde_json::json!({
                "t": t,
                "verify_url": format!("{base_url}/verify-email?token=abc123testtoken"),
                "base_url": base_url,
            }),
            Self::VerifyEmailChange => serde_json::json!({
                "t": t,
                "verify_url": format!("{base_url}/verify-email-change?token=abc123testtoken"),
            }),
            Self::PasswordReset => serde_json::json!({
                "t": t,
                "reset_url": format!("{base_url}/reset-password?token=abc123testtoken"),
            }),
        }
    }
}

#[actix_web::main]
async fn main() {
    dotenvy::dotenv().ok();

    let t: serde_json::Value = std::fs::read_to_string("locales/en.json")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let tpl_name = TEMPLATE.path();
    let tpl_engine = match full_stack_engine::testing::load_themes(starter::themes()) {
        Ok(tera) => tera,
        Err(e) => {
            eprintln!("Failed to load the theme templates: {e}");
            std::process::exit(1);
        }
    };

    let ctx = match tera::Context::from_serialize(TEMPLATE.context(t.clone())) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to build template context: {e}");
            std::process::exit(1);
        }
    };

    let body = match tpl_engine.render(tpl_name, &ctx) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Template rendering failed: {e}");
            std::process::exit(1);
        }
    };

    let subject = TEMPLATE.subject(&t);
    println!("Sending \"{subject}\" to {TO_EMAIL} ...");

    // SMTP settings now come from the validated config rather than being read
    // ad hoc at send time, so this reports a misconfiguration the same way the
    // server would at boot.
    let config = match full_stack_engine::config::Config::from_env() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    };

    match send_mail(&config, TO_EMAIL, &subject, &body).await {
        Ok(()) => println!("Done \u{2014} email sent successfully."),
        Err(e) => {
            eprintln!("SMTP error: {e}");
            std::process::exit(1);
        }
    }
}
