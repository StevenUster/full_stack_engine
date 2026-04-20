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

    let parts: Vec<&str> = smtp_host.split(':').collect();
    let host = parts[0];
    let port = parts.get(1).and_then(|p| p.parse::<u16>().ok());

    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::relay(host)
        .map_err(|e| format!("SMTP relay configuration error: {e}"))?;

    if let Some(p) = port {
        builder = builder.port(p);
    }

    let mailer = if port == Some(465) {
        let tls_parameters = TlsParameters::new(host.to_string())?;
        builder.tls(Tls::Wrapper(tls_parameters))
    } else {
        if port == Some(587) {
            let tls_parameters = TlsParameters::new(host.to_string())?;
            builder.tls(Tls::Required(tls_parameters))
        } else {
            builder
        }
    }
    .credentials(creds)
    .build();

    if let Err(e) = mailer.send(email).await {
        error!("SMTP send error: {e}");
        return Err(format!("Connection error: {e}").into());
    }

    Ok(())
}
