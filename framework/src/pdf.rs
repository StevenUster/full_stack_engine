//! HTML → PDF rendering with headless Chromium (`pdf` feature).
//!
//! Invoices, receipts, certificates, tickets and reports are all "render the
//! page you already have, as a file". The framework already owns the template
//! engine, so the only missing piece was the browser — and the browser is the
//! part that is easy to get wrong:
//!
//! * **One instance, reused.** Launching Chromium per render costs hundreds of
//!   milliseconds and a few hundred megabytes; a batch of fifty certificates
//!   launching fifty browsers takes the process down.
//! * **Renders are serialized.** With the instance shared, only one page is
//!   open at a time, so memory stays bounded no matter how large the batch.
//! * **Every render is bounded by a timeout.** A template with an endless
//!   resource must not hang the one shared browser and block everything behind
//!   it.
//! * **Subresources are restricted to `data:` and `about:`.** This is the
//!   security property that matters most, and the reason this belongs in the
//!   framework rather than in each app: a template is often
//!   attacker-influenced (an uploaded letter template, a user-supplied name),
//!   and Chromium will happily fetch `file:///etc/passwd` into an `<img>` or
//!   reach an internal address through CSS `url()`. Self-contained documents —
//!   inline CSS, `data:` images, [`crate::qr`] codes — are unaffected.
//! * **A failed render drops the instance,** so a browser that died does not
//!   poison every later call.
//!
//! Chromium is located by `chromiumoxide`'s auto-detection, which honours the
//! `CHROME` environment variable. Install it in the runtime image and point
//! `CHROME` at it:
//!
//! ```dockerfile
//! RUN apt-get update && apt-get install -y chromium && rm -rf /var/lib/apt/lists/*
//! ENV CHROME=/usr/bin/chromium
//! ```
//!
//! ```ignore
//! let html = data.render_email("invoices/receipt", &ctx).await?;
//! let bytes = pdf::render_html(&html, &pdf::PageSetup::A4).await?;
//! mail::send_mail_with_attachments(
//!     &data.config, to, "Your receipt", &html,
//!     vec![MailAttachment::pdf("receipt.pdf", bytes)],
//! ).await?;
//! ```

use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EnableParams, EventRequestPaused, FailRequestParams,
};
use chromiumoxide::cdp::browser_protocol::network::ErrorReason;
use chromiumoxide::cdp::browser_protocol::page::PrintToPdfParams;
use futures::StreamExt;
use tokio::sync::Mutex;
use tracing::error;

use crate::error::{AppError, ErrorContext};

static BROWSER: Mutex<Option<Browser>> = Mutex::const_new(None);

/// Upper bound on a single render. Generous enough for a long document with
/// embedded images, short enough that a broken template does not block the
/// shared browser indefinitely.
const RENDER_TIMEOUT: Duration = Duration::from_secs(30);

/// Physical page setup. Sizes are in inches because that is what the CDP
/// print command takes.
#[derive(Copy, Clone, Debug)]
pub struct PageSetup {
    pub width_in: f64,
    pub height_in: f64,
    /// Margins in inches, clockwise from the top.
    pub margins_in: [f64; 4],
    /// Whether a `@page { size: ... }` rule in the document overrides the
    /// above. On by default: a template that states its own size means it.
    pub prefer_css_page_size: bool,
    pub landscape: bool,
    pub print_background: bool,
}

impl PageSetup {
    /// A4, edge to edge.
    ///
    /// No margins, because a template built for print lays out its own
    /// (`.page { margin: 0 }` under `@media print`). Chromium's default is US
    /// Letter plus ~1 cm on every side, which is both narrower and ~4 cm
    /// shorter than A4 — enough to push the end of a one-page document onto a
    /// second page that is then almost empty.
    pub const A4: Self = Self {
        width_in: 210.0 / 25.4,
        height_in: 297.0 / 25.4,
        margins_in: [0.0; 4],
        prefer_css_page_size: true,
        landscape: false,
        print_background: true,
    };

    /// US Letter, edge to edge.
    pub const LETTER: Self = Self {
        width_in: 8.5,
        height_in: 11.0,
        ..Self::A4
    };

    /// The same page, rotated.
    #[must_use]
    pub const fn landscape(self) -> Self {
        Self {
            landscape: true,
            ..self
        }
    }

    /// The same page with a uniform margin, in millimetres — for a document
    /// that does *not* set its own.
    #[must_use]
    pub fn margin_mm(self, mm: f64) -> Self {
        let inches = mm / 25.4;
        Self {
            margins_in: [inches; 4],
            prefer_css_page_size: false,
            ..self
        }
    }
}

impl Default for PageSetup {
    fn default() -> Self {
        Self::A4
    }
}

impl From<PageSetup> for PrintToPdfParams {
    fn from(setup: PageSetup) -> Self {
        let [top, right, bottom, left] = setup.margins_in;
        PrintToPdfParams {
            print_background: Some(setup.print_background),
            prefer_css_page_size: Some(setup.prefer_css_page_size),
            landscape: Some(setup.landscape),
            paper_width: Some(setup.width_in),
            paper_height: Some(setup.height_in),
            margin_top: Some(top),
            margin_right: Some(right),
            margin_bottom: Some(bottom),
            margin_left: Some(left),
            ..PrintToPdfParams::default()
        }
    }
}

/// Renders a complete HTML document to PDF bytes.
///
/// The document must be self-contained: inline CSS and `data:` URIs for
/// images and fonts. Every other subresource request is blocked — see the
/// module docs for why.
///
/// # Errors
///
/// Returns [`AppError::Internal`] if Chromium cannot be launched or the render
/// fails or times out. The shared instance is dropped on any failure, so the
/// next call starts a fresh browser.
pub async fn render_html(html: &str, setup: &PageSetup) -> Result<Vec<u8>, AppError> {
    let mut guard = BROWSER.lock().await;
    if guard.is_none() {
        *guard = Some(launch().await?);
    }

    let result = match guard.as_ref() {
        Some(browser) => {
            match tokio::time::timeout(RENDER_TIMEOUT, render_with(browser, html, setup)).await {
                Ok(res) => res,
                Err(_) => Err(AppError::Internal("pdf render timed out".to_string())),
            }
        }
        None => Err(AppError::Internal("chromium unavailable".to_string())),
    };

    if result.is_err()
        && let Some(mut browser) = guard.take()
    {
        let _ = browser.close().await;
    }

    result
}

/// Renders a template from the app's theme stack straight to PDF.
///
/// The template receives the same context a page would, so an invoice can be
/// previewed in a browser at a route and attached to a mail from the same
/// file.
///
/// # Errors
///
/// Returns [`AppError::Internal`] if the template fails to render, or whatever
/// [`render_html`] returns.
pub async fn render_template<T: serde::Serialize>(
    data: &crate::AppData,
    template: &str,
    context: &T,
    setup: &PageSetup,
) -> Result<Vec<u8>, AppError> {
    let html = data
        .render_email(template, context)
        .await
        .map_err(|e| AppError::Internal(format!("rendering {template} for pdf: {e}")))?;
    render_html(&html, setup).await
}

async fn launch() -> Result<Browser, AppError> {
    // A unique profile directory per launch. chromiumoxide otherwise reuses one
    // fixed temp directory, whose Chromium `SingletonLock` survives an unclean
    // shutdown and then blocks every future launch — including a concurrent one
    // from a second process on the same host.
    let user_data_dir = std::env::temp_dir().join(format!("fse-chromium-{}", uuid::Uuid::new_v4()));

    let config = BrowserConfig::builder()
        .no_sandbox()
        .user_data_dir(user_data_dir)
        // Keeps Chromium alive inside a container: no GPU to talk to, and
        // /dev/shm is typically 64 MB, which it will otherwise exhaust.
        .arg("--disable-gpu")
        .arg("--disable-dev-shm-usage")
        .build()
        // chromiumoxide's builder fails with a bare `String`, so there is no
        // cause chain for `.context()` to preserve here.
        .map_err(|e| AppError::Internal(format!("chromium config: {e}")))?;

    let (browser, mut handler) = Browser::launch(config).await.context("launch chromium")?;

    // The CDP handler must be polled continuously or the browser stops
    // responding; run it detached for the instance's lifetime.
    //
    // Plain `tokio::spawn`, **not** `actix_web::rt::spawn`: PDFs are also
    // generated from cron jobs, which run on a plain Tokio task with no Actix
    // arbiter or `LocalSet` — `actix_web::rt::spawn` panics outright there.
    tokio::spawn(async move {
        while let Some(event) = handler.next().await {
            if let Err(e) = event {
                error!("chromium handler stopped: {e}");
                break;
            }
        }
    });

    Ok(browser)
}

async fn render_with(
    browser: &Browser,
    html: &str,
    setup: &PageSetup,
) -> Result<Vec<u8>, AppError> {
    let page = browser
        .new_page("about:blank")
        .await
        .context("open chromium page")?;

    page.execute(EnableParams::default())
        .await
        .context("enable request interception")?;

    let mut paused = page
        .event_listener::<EventRequestPaused>()
        .await
        .context("listen for paused requests")?;
    let interceptor_page = page.clone();
    let interceptor = tokio::spawn(async move {
        while let Some(event) = paused.next().await {
            let url = event.request.url.as_str();
            // Allow-list, not a block-list: a block-list has to anticipate
            // every scheme Chromium supports, and `file:`/`http:` are only two
            // of them.
            let allowed = url.starts_with("data:") || url.starts_with("about:");
            let request_id = event.request_id.clone();
            let _ = if allowed {
                interceptor_page
                    .execute(ContinueRequestParams::new(request_id))
                    .await
                    .map(|_| ())
            } else {
                interceptor_page
                    .execute(FailRequestParams::new(
                        request_id,
                        ErrorReason::BlockedByClient,
                    ))
                    .await
                    .map(|_| ())
            };
        }
    });

    let render = async {
        page.set_content(html).await.context("set page content")?;
        page.pdf(PrintToPdfParams::from(*setup))
            .await
            .context("print pdf")
    };
    let result = render.await;

    interceptor.abort();
    // Best effort: failing to close a page must not fail a completed render.
    let _ = page.close().await;

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page setup is what a template's layout depends on, and it is pure
    /// arithmetic — worth pinning without needing a browser.
    #[test]
    fn a4_is_a4_and_carries_no_margins() {
        let params = PrintToPdfParams::from(PageSetup::A4);
        assert!((params.paper_width.unwrap() - 8.267_716).abs() < 1e-4);
        assert!((params.paper_height.unwrap() - 11.692_913).abs() < 1e-4);
        for margin in [
            params.margin_top,
            params.margin_right,
            params.margin_bottom,
            params.margin_left,
        ] {
            assert_eq!(margin, Some(0.0));
        }
        assert_eq!(params.prefer_css_page_size, Some(true));
        assert_eq!(params.print_background, Some(true));
        assert_eq!(params.landscape, Some(false));
    }

    #[test]
    fn a_uniform_margin_stops_deferring_to_the_documents_own_page_size() {
        // Otherwise the margin would be silently ignored for exactly the
        // documents that need one — those that set no `@page` rule.
        let setup = PageSetup::A4.margin_mm(20.0);
        assert!(!setup.prefer_css_page_size);
        let params = PrintToPdfParams::from(setup);
        assert!((params.margin_left.unwrap() - 20.0 / 25.4).abs() < 1e-6);
        assert_eq!(params.prefer_css_page_size, Some(false));
    }

    #[test]
    fn landscape_keeps_the_paper_size() {
        let setup = PageSetup::A4.landscape();
        assert!(setup.landscape);
        assert!((setup.width_in - PageSetup::A4.width_in).abs() < f64::EPSILON);
        assert_eq!(PrintToPdfParams::from(setup).landscape, Some(true));
    }

    #[test]
    fn letter_differs_from_a4() {
        assert!((PageSetup::LETTER.width_in - 8.5).abs() < f64::EPSILON);
        assert!((PageSetup::LETTER.height_in - 11.0).abs() < f64::EPSILON);
    }
}
