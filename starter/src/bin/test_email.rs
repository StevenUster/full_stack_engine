//! Renders one of the app's email templates with sample data and real
//! translations, and optionally sends it:
//!
//!   cargo run --bin test_email                 # print the HTML
//!   cargo run --bin test_email you@example.com # and send it
//!
//! Needs the themes built (`bun run build` in themes/*/), since the mail renders
//! from the same theme stack the app serves pages from. Sending additionally
//! needs SMTP_* in `.env`.
//!
//! Everything else — config validation, locale layering, theme loading, the
//! SMTP transport — is `full_stack_engine::dev::preview_mail`.

use full_stack_engine::prelude::serde_json;

/// Pick the template to preview.
const TEMPLATE: Template = Template::Verify;

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
        match self {
            Self::Verify => &t["verify_email"]["subject"],
            Self::VerifyEmailChange => &t["verify_email_change"]["subject"],
            Self::PasswordReset => &t["password_reset_email"]["subject"],
        }
        .as_str()
        .unwrap_or("Test Email")
        .to_string()
    }

    /// Sample data, in the shape the real sender passes.
    fn context(self, t: &serde_json::Value, base_url: &str) -> serde_json::Value {
        let token = "abc123testtoken";
        match self {
            Self::Verify => serde_json::json!({
                "t": t,
                "verify_url": format!("{base_url}/verify-email?token={token}"),
            }),
            Self::VerifyEmailChange => serde_json::json!({
                "t": t,
                "verify_url": format!("{base_url}/verify-email-change?token={token}"),
            }),
            Self::PasswordReset => serde_json::json!({
                "t": t,
                "reset_url": format!("{base_url}/reset-password?token={token}"),
            }),
        }
    }
}

#[actix_web::main]
async fn main() -> Result<(), String> {
    let to = std::env::args().nth(1);

    let html = full_stack_engine::dev::preview_mail(
        starter::themes(),
        "en",
        Some(&starter::LOCALES_DIR),
        TEMPLATE.path(),
        |t, base_url| (TEMPLATE.subject(t), TEMPLATE.context(t, base_url)),
        to.as_deref(),
    )
    .await?;

    if to.is_none() {
        println!("{html}");
    }
    Ok(())
}
