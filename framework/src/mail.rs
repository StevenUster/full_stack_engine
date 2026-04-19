use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::header::ContentType,
    transport::smtp::authentication::Credentials,
    transport::smtp::client::{Tls, TlsParameters},
};
use log::error;
use std::env;

pub async fn send_mail(
    to: &str,
    subject: &str,
    body: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let smtp_host = env::var("SMTP_HOST").unwrap_or_default().trim().to_string();
    let smtp_user = env::var("SMTP_USER").unwrap_or_default().trim().to_string();
    let smtp_pass = env::var("SMTP_PASS").unwrap_or_default().trim().to_string();

    if smtp_host.is_empty() || smtp_user.is_empty() {
        return Err("SMTP configuration is missing".into());
    }

    log::debug!("Sending mail to {} via host {}", to, smtp_host);

    let email = Message::builder()
        .from(smtp_user.parse()?)
        .to(to.parse()?)
        .subject(subject)
        .header(ContentType::TEXT_HTML)
        .body(body.to_string())?;

    let creds = Credentials::new(smtp_user, smtp_pass);

    let mailer = if smtp_host.contains(":465") {
        // Port 465 uses Implicit TLS (SMTPS)
        let host = smtp_host.split(':').next().unwrap_or(&smtp_host);
        let tls_parameters = TlsParameters::new(host.to_string())?;
        AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp_host)
            .map_err(|e| format!("SMTP TLS configuration error: {e}"))?
            .tls(Tls::Wrapper(tls_parameters))
            .credentials(creds)
            .build()
    } else {
        // Port 587 and 25 typically use STARTTLS (Explicit TLS)
        AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp_host)
            .map_err(|e| format!("SMTP relay configuration error: {e}"))?
            .credentials(creds)
            .build()
    };

    if let Err(e) = mailer.send(email).await {
        error!("SMTP send error: {e}");
        return Err(format!("Connection error: {e}").into());
    }

    Ok(())
}
