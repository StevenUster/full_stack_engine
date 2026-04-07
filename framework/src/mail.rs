use lettre::{
    Message, SmtpTransport, Transport,
    message::header::ContentType,
    transport::smtp::authentication::Credentials,
};
use std::env;

pub fn send_mail(
    to: &str,
    subject: &str,
    body: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let smtp_host = env::var("SMTP_HOST").unwrap_or_default();
    let smtp_user = env::var("SMTP_USER").unwrap_or_default();
    let smtp_pass = env::var("SMTP_PASS").unwrap_or_default();

    if smtp_host.is_empty() || smtp_user.is_empty() {
        return Err("SMTP configuration is missing".into());
    }

    let email = Message::builder()
        .from(smtp_user.parse()?)
        .to(to.parse()?)
        .subject(subject)
        .header(ContentType::TEXT_HTML)
        .body(body.to_string())?;

    let creds = Credentials::new(smtp_user, smtp_pass);

    let mailer = SmtpTransport::relay(&smtp_host)?
        .credentials(creds)
        .build();

    mailer.send(&email)?;

    Ok(())
}
