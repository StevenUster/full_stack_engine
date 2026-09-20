use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::header::ContentType,
    transport::smtp::authentication::Credentials,
    transport::smtp::client::{Tls, TlsParameters},
};
use secrecy::ExposeSecret;
use tracing::error;

use crate::config::{Config, SmtpConfig};

/// Sends an HTML email through the app's configured SMTP server.
///
/// The configuration is resolved and validated at boot (see
/// [`crate::config`]) rather than read from the environment here, so a typo in
/// `SMTP_HOST` fails the deployment instead of surfacing weeks later as a
/// password reset that silently did nothing.
///
/// # Errors
///
/// Returns an error if no mailer is configured, if an address does not parse,
/// or if the SMTP conversation fails.
pub async fn send_mail(
    cfg: &Config,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let smtp = cfg
        .smtp
        .as_ref()
        .ok_or("SMTP is not configured (set SMTP_HOST, SMTP_USER and SMTP_PASS)")?;

    // The recipient is logged, the credentials are not — `password` is a
    // `SecretString` and has no `Display` to accidentally interpolate.
    tracing::debug!(smtp.host = %smtp.host, "sending mail to {to}");

    let email = Message::builder()
        .from(smtp.user.parse()?)
        .to(to.parse()?)
        .subject(subject)
        .header(ContentType::TEXT_HTML)
        .body(body.to_string())?;

    let mailer = build_transport(smtp)?;

    if let Err(e) = mailer.send(email).await {
        error!("SMTP send error: {e}");
        return Err(format!("Connection error: {e}").into());
    }

    Ok(())
}

/// Builds the transport for `smtp`, requiring TLS on the two standard
/// submission ports.
fn build_transport(
    smtp: &SmtpConfig,
) -> Result<AsyncSmtpTransport<Tokio1Executor>, Box<dyn std::error::Error + Send + Sync>> {
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp.host)
        .map_err(|e| format!("SMTP relay configuration error: {e}"))?;

    if let Some(port) = smtp.port {
        builder = builder.port(port);
    }

    let builder = match smtp.port {
        // Implicit TLS from the first byte.
        Some(465) => builder.tls(Tls::Wrapper(TlsParameters::new(smtp.host.clone())?)),
        // STARTTLS, and required rather than opportunistic: an attacker who can
        // strip the upgrade must not be handed the credentials in the clear.
        Some(587) => builder.tls(Tls::Required(TlsParameters::new(smtp.host.clone())?)),
        _ => builder,
    };

    Ok(builder
        .credentials(Credentials::new(
            smtp.user.clone(),
            smtp.password.expose_secret().to_string(),
        ))
        .build())
}
