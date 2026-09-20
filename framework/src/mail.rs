use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::header::ContentType,
    message::{MultiPart, SinglePart},
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
    send_mail_with_attachments(cfg, to, subject, body, Vec::new()).await
}

/// One file attached to an outgoing mail.
pub struct MailAttachment {
    /// Shown to the recipient — keep it free of path separators.
    pub filename: String,
    /// e.g. `application/pdf`.
    pub content_type: String,
    pub bytes: Vec<u8>,
}

impl MailAttachment {
    /// A PDF attachment, the common case (invoices, certificates, letters).
    #[must_use]
    pub fn pdf(filename: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            filename: filename.into(),
            content_type: "application/pdf".to_string(),
            bytes,
        }
    }
}

/// Sends an HTML email with files attached.
///
/// Exists so that an app needing to attach a generated PDF does not rebuild the
/// SMTP transport itself. That matters beyond convenience: the transport is
/// where the TLS policy lives (see [`build_transport`]), and a second copy of it
/// in an app is a place for "required" to quietly become "opportunistic".
///
/// # Errors
///
/// Returns an error if no mailer is configured, if an address or content type
/// does not parse, or if the SMTP conversation fails.
pub async fn send_mail_with_attachments(
    cfg: &Config,
    to: &str,
    subject: &str,
    body: &str,
    attachments: Vec<MailAttachment>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let smtp = cfg
        .smtp
        .as_ref()
        .ok_or("SMTP is not configured (set SMTP_HOST, SMTP_USER and SMTP_PASS)")?;

    // The recipient is logged, the credentials are not — `password` is a
    // `SecretString` and has no `Display` to accidentally interpolate.
    tracing::debug!(
        smtp.host = %smtp.host,
        attachments = attachments.len(),
        "sending mail to {to}"
    );

    let builder = Message::builder()
        .from(smtp.user.parse()?)
        .to(to.parse()?)
        .subject(subject);

    let email = if attachments.is_empty() {
        builder
            .header(ContentType::TEXT_HTML)
            .body(body.to_string())?
    } else {
        let mut part = MultiPart::mixed().singlepart(SinglePart::html(body.to_string()));
        for attachment in attachments {
            let content_type = ContentType::parse(&attachment.content_type)?;
            part = part.singlepart(
                lettre::message::Attachment::new(attachment.filename)
                    .body(attachment.bytes, content_type),
            );
        }
        builder.multipart(part)?
    };

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
