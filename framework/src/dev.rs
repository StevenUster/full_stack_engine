//! The `cargo run --bin dev` development runner.
//!
//! Starts the theme's Astro dev server and the backend with hot reload, and
//! stops both together. Every app had a byte-identical copy of this apart from
//! its own binary name, so it lives here now:
//!
//! ```ignore
//! // src/bin/dev.rs
//! fn main() -> std::io::Result<()> {
//!     full_stack_engine::dev::run(env!("CARGO_PKG_NAME"))
//! }
//! ```
//!
//! `env!("CARGO_PKG_NAME")` rather than a lookup: the framework cannot know
//! which crate called it, and hard-coding the name in a string is the thing
//! that goes stale when an app is renamed.
//!
//! Requires `bun` and `cargo-watch` (`cargo install cargo-watch`) on `PATH`.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

/// How the dev runner starts the two halves of the app. [`Default`] matches
/// the layout the starter uses; override a field only if the app moved
/// something.
pub struct DevServer {
    /// The app's binary target, i.e. `env!("CARGO_PKG_NAME")`.
    pub bin: String,
    /// The child theme's directory, relative to the manifest.
    pub theme_dir: String,
    /// The theme dev server's port, for the message printed at startup. The
    /// port itself comes from the theme's own config.
    pub theme_port: u16,
    /// How long to let the theme dev server boot before starting the backend.
    /// The backend proxies to it in dev, so starting first means the first
    /// page load misses.
    pub theme_startup: std::time::Duration,
}

impl Default for DevServer {
    fn default() -> Self {
        Self {
            bin: String::new(),
            theme_dir: "theme".to_string(),
            theme_port: 4321,
            theme_startup: std::time::Duration::from_secs(2),
        }
    }
}

/// Starts both dev servers with the default layout and blocks until the
/// backend exits.
///
/// # Errors
///
/// Returns an error if `bun`/`cargo watch` cannot be started, if the initial
/// theme install or build fails, or if `ENV` is not `dev`.
pub fn run(bin: &str) -> io::Result<()> {
    DevServer {
        bin: bin.to_string(),
        ..DevServer::default()
    }
    .start()
}

impl DevServer {
    /// Starts both dev servers and blocks until the backend exits, then stops
    /// the theme server.
    ///
    /// # Errors
    ///
    /// See [`run`].
    ///
    /// # Panics
    ///
    /// Panics if the Ctrl-C handler cannot be installed, which means the
    /// process could not stop its children cleanly — better to fail now than
    /// to leave an orphaned dev server behind on every exit.
    pub fn start(&self) -> io::Result<()> {
        dotenvy::dotenv().ok();

        // Not a warning: with `ENV=prod` the backend serves the *built* theme
        // and ignores the dev server entirely, so everything would appear to
        // work while no edit showed up.
        let env_mode = std::env::var("ENV").unwrap_or_else(|_| "prod".to_string());
        if env_mode != "dev" {
            eprintln!("⚠️  ENV is not set to 'dev'. Please set ENV=dev in your .env file.");
            std::process::exit(1);
        }

        println!("🚀 Starting development servers...");

        let theme = Path::new(&self.theme_dir);
        if !theme.join("node_modules").exists() {
            println!("📦 Installing theme dependencies...");
            status(Command::new("bun").arg("install").current_dir(theme))?;
        }
        // The binary embeds `<theme>/dist` — the fallback for anything the dev
        // server doesn't serve — so it has to exist before the first build.
        if !theme.join("dist").exists() {
            println!("🎨 Building the theme once...");
            status(
                Command::new("bun")
                    .args(["run", "build"])
                    .current_dir(theme),
            )?;
        }

        println!(
            "🎨 Starting the theme dev server (Astro) on port {}...",
            self.theme_port
        );
        let mut theme_child = Command::new("bun")
            .arg("dev")
            .current_dir(theme)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;

        std::thread::sleep(self.theme_startup);

        println!("🦀 Starting the backend with hot reload...");
        let mut backend = Command::new("cargo")
            .args([
                "watch",
                "-i",
                &format!("{}/", self.theme_dir),
                "-x",
                &format!("run --bin {}", self.bin),
            ])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;

        ctrlc::set_handler(|| {
            println!("\n🛑 Stopping development servers...");
            std::process::exit(0);
        })
        .expect("Error setting Ctrl-C handler");

        let backend_status = backend.wait()?;

        theme_child.kill()?;
        theme_child.wait()?;

        if !backend_status.success() {
            std::process::exit(backend_status.code().unwrap_or(1));
        }
        Ok(())
    }
}

fn status(cmd: &mut Command) -> io::Result<()> {
    if cmd.status()?.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{cmd:?} failed")))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Mail preview
// ─────────────────────────────────────────────────────────────────────────────

/// Renders one email template the way the running app would, and optionally
/// sends it.
///
/// Every app had a `bin/test_email.rs` that was four fifths framework: read
/// `.env`, validate the config, layer the framework's locale files under the
/// app's, load the theme stack, render, send. Only the template list and its
/// sample data are the app's, and those stay in the app.
///
/// With `to` set to `None` the rendered HTML is returned and nothing is sent,
/// which is the fast loop while writing a template.
///
/// `build` receives the resolved translations and the app's base URL, and
/// returns the subject and the render context — both in one closure because
/// the subject usually comes out of the same translation tree.
///
/// ```ignore
/// // src/bin/test_email.rs
/// #[actix_web::main]
/// async fn main() -> Result<(), String> {
///     let html = full_stack_engine::dev::preview_mail(
///         myapp::themes(), "de", Some(&myapp::LOCALES_DIR), "emails/verify",
///         |t, base_url| (
///             t["verify_email"]["subject"].as_str().unwrap_or("").to_string(),
///             serde_json::json!({ "t": t, "verify_url": format!("{base_url}/verify") }),
///         ),
///         std::env::args().nth(1).as_deref(),
///     ).await?;
///     println!("{html}");
///     Ok(())
/// }
/// ```
///
/// # Errors
///
/// Returns the problem as a string: an invalid configuration (every problem at
/// once, as at boot), a theme that does not resolve, a template that does not
/// render, or an SMTP failure.
pub async fn preview_mail(
    themes: Vec<crate::themes::Theme>,
    lang: &str,
    locales: Option<&'static include_dir::Dir<'static>>,
    template: &str,
    build: impl FnOnce(&serde_json::Value, &str) -> (String, serde_json::Value),
    to: Option<&str>,
) -> Result<String, String> {
    dotenvy::dotenv().ok();

    // The same validated read the server does, so a broken `SMTP_HOST` is
    // reported here exactly as it would be at boot rather than as a confusing
    // send failure.
    let config = crate::config::Config::from_env().map_err(|e| e.to_string())?;

    // Same layering as the app: framework base translations < app files.
    let translations = crate::i18n::resolve_locales(crate::i18n::build_locales(&[], locales), lang)
        .remove(lang)
        .unwrap_or_default();

    // The strict loader: a broken template is an error here, not a skipped
    // page as it would be at boot.
    let tera = crate::testing::load_themes_localized(themes, lang, config.currency.as_deref())?;

    let base_url = config.base_url();
    let (subject, context) = build(&translations, &base_url);
    let mut ctx = tera::Context::from_serialize(&context).map_err(|e| e.to_string())?;
    ctx.insert("base_url", &base_url);

    let html = tera
        .render(template, &ctx)
        .map_err(|e| format!("rendering {template}: {}", error_chain(&e)))?;

    if let Some(to) = to {
        crate::mail::send_mail(&config, to, &subject, &html)
            .await
            .map_err(|e| format!("sending to {to}: {e}"))?;
        println!("Sent `{template}` to {to}");
    }

    Ok(html)
}

fn error_chain(err: &tera::Error) -> String {
    use std::fmt::Write as _;
    let mut msg = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        let _ = write!(msg, "\n  caused by: {cause}");
        source = cause.source();
    }
    msg
}
