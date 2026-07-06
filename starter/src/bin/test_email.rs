//! Send a test email using a real email template rendered with fake data + real translations.
//!
//! Configure the two constants below, then run:
//!   cargo run --bin test_email
//!
//! Prerequisites:
//!   - Astro dev server running: `bun dev` inside src/frontend/
//!   - SMTP_* env vars set in .env

// ── Configure here ──────────────────────────────────────────────────────────
const TO_EMAIL: &str = "you@example.com";
const TEMPLATE: Template = Template::Verify;
// ────────────────────────────────────────────────────────────────────────────

use full_stack_engine::mail::send_mail;
use full_stack_engine::prelude::{reqwest, serde_json, tera};

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

/// Remove Astro dev-server artifacts (Tailwind CSS blob, Vite HMR scripts,
/// dev-toolbar JS) so only the actual email HTML is sent.
fn strip_dev_artifacts(html: &str) -> String {
    let no_style = strip_tags(html, "<style", "</style>");
    strip_tags(&no_style, "<script", "</script>")
}

fn strip_tags(html: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find(open) {
        out.push_str(&rest[..start]);
        rest = match rest[start..].find(close) {
            Some(end) => &rest[start + end + close.len()..],
            None => break,
        };
    }
    out.push_str(rest);
    out
}

#[actix_web::main]
async fn main() {
    dotenv::dotenv().ok();

    let t: serde_json::Value = std::fs::read_to_string("locales/en.json")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let url = format!("http://localhost:4321/{}", TEMPLATE.path());
    println!("Fetching template from {url} ...");

    let html = match reqwest::get(&url).await {
        Ok(res) if res.status().is_success() => match res.text().await {
            Ok(text) => strip_dev_artifacts(&text),
            Err(e) => {
                eprintln!("Failed to read response body: {e}");
                std::process::exit(1);
            }
        },
        Ok(res) => {
            eprintln!("Astro dev server returned HTTP {}", res.status());
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Could not reach Astro dev server at {url}: {e}");
            eprintln!("Make sure `bun dev` is running inside src/frontend/");
            std::process::exit(1);
        }
    };

    let tpl_name = TEMPLATE.path();
    let mut tpl_engine = tera::Tera::default();
    if let Err(e) = tpl_engine.add_raw_template(tpl_name, &html) {
        eprintln!("Failed to parse template: {e}");
        std::process::exit(1);
    }

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

    match send_mail(TO_EMAIL, &subject, &body).await {
        Ok(()) => println!("Done \u{2014} email sent successfully."),
        Err(e) => {
            eprintln!("SMTP error: {e}");
            std::process::exit(1);
        }
    }
}
