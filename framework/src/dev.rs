//! The `cargo run --bin dev` development runner.
//!
//! Starts the active theme's dev server (Astro) and the backend with hot
//! reload, and stops both together. Every app had a byte-identical copy of
//! this apart from its own binary name, so it lives here now:
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
//! The themes are the app's `themes/` folder (see [`crate::themes!`]). The
//! binary embeds every one of them, so every npm theme is installed and
//! built once if it never was; only the active theme's dev server runs.
//!
//! Requires `bun` and `cargo-watch` (`cargo install cargo-watch`) on `PATH`.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// How the dev runner starts the two halves of the app. [`Default`] reads
/// the layout from `fse.toml` (`[themes] dir`, default `themes`); override a
/// field only if the app keeps its themes elsewhere.
pub struct DevServer {
    /// The app's binary target, i.e. `env!("CARGO_PKG_NAME")`.
    pub bin: String,
    /// The app's themes folder, relative to the manifest.
    pub themes_dir: String,
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
            themes_dir: fse_toml_themes()
                .and_then(|t| t.get("dir")?.as_str().map(str::to_string))
                .unwrap_or_else(|| "themes".to_string()),
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
/// Returns an error if `bun`/`cargo watch` cannot be started, if a theme's
/// install or build fails, if the active theme can't be told, or if `ENV` is
/// not `dev`.
pub fn run(bin: &str) -> io::Result<()> {
    DevServer {
        bin: bin.to_string(),
        ..DevServer::default()
    }
    .start()
}

/// One folder of the themes directory.
#[derive(Debug)]
struct LocalTheme {
    name: String,
    parent: Option<String>,
    dir: PathBuf,
    /// Has a `package.json`: built by npm, with a dev server.
    npm: bool,
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

        let themes = local_themes(Path::new(&self.themes_dir))?;
        prepare(&themes)?;

        let configured = std::env::var("THEME")
            .ok()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| {
                fse_toml_themes().and_then(|t| t.get("active")?.as_str().map(str::to_string))
            });
        let active = active_theme(&themes, configured.as_deref())?;

        let mut theme_child: Option<Child> = None;
        match active.and_then(|name| themes.iter().find(|t| t.name == name)) {
            Some(theme) if theme.npm => {
                println!(
                    "🎨 Starting the dev server of theme \"{}\" (Astro) on port {}...",
                    theme.name, self.theme_port
                );
                theme_child = Some(
                    Command::new("bun")
                        .arg("dev")
                        .current_dir(&theme.dir)
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::inherit())
                        .spawn()?,
                );
                std::thread::sleep(self.theme_startup);
            }
            _ => println!(
                "🎨 The active theme ({}) has no dev server here — serving its built files.",
                active.unwrap_or("none")
            ),
        }

        println!("🦀 Starting the backend with hot reload...");
        let mut backend = Command::new("cargo")
            .args([
                "watch",
                "-i",
                &format!("{}/", self.themes_dir.trim_end_matches('/')),
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

        if let Some(mut theme_child) = theme_child {
            theme_child.kill()?;
            theme_child.wait()?;
        }

        if !backend_status.success() {
            std::process::exit(backend_status.code().unwrap_or(1));
        }
        Ok(())
    }
}

/// `[themes]` of `./fse.toml`, if there is one.
fn fse_toml_themes() -> Option<toml::Table> {
    let raw = std::fs::read_to_string("fse.toml").ok()?;
    let mut value: toml::Table = raw.parse().ok()?;
    match value.remove("themes")? {
        toml::Value::Table(table) => Some(table),
        _ => None,
    }
}

/// Every theme folder of `dir` (name order), read from its own `theme.json`.
fn local_themes(dir: &Path) -> io::Result<Vec<LocalTheme>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    let mut themes = Vec::new();
    for dir in dirs {
        let Ok(raw) = std::fs::read(dir.join(crate::themes::MANIFEST_FILE)) else {
            continue;
        };
        let manifest: crate::themes::ThemeManifest = serde_json::from_slice(&raw).map_err(|e| {
            io::Error::other(format!("{}/theme.json is invalid: {e}", dir.display()))
        })?;
        themes.push(LocalTheme {
            name: manifest.name,
            parent: manifest.parent,
            npm: dir.join("package.json").is_file(),
            dir,
        });
    }
    Ok(themes)
}

/// Installs every npm theme that has no `node_modules`, then builds every
/// one without a built `dist/` — the binary embeds all of them. Installs
/// come first because a child's build reads its local parent's sources (and
/// their dependencies); builds go parent first, so a broken parent fails
/// before the children that build its pages.
fn prepare(themes: &[LocalTheme]) -> io::Result<()> {
    for theme in themes.iter().filter(|t| t.npm) {
        if !theme.dir.join("node_modules").exists() {
            println!(
                "📦 Installing the dependencies of theme \"{}\"...",
                theme.name
            );
            status(Command::new("bun").arg("install").current_dir(&theme.dir))?;
        }
    }
    for theme in parents_first(themes) {
        if theme.npm && !theme.dir.join("dist").join("theme.json").exists() {
            println!("🎨 Building theme \"{}\" once...", theme.name);
            status(
                Command::new("bun")
                    .args(["run", "build"])
                    .current_dir(&theme.dir),
            )?;
            // The app was compiled with this theme as an unbuilt stub, which
            // tracks only its `theme.json`: touching it makes the backend's
            // next build embed `dist/`.
            std::fs::File::options()
                .append(true)
                .open(theme.dir.join(crate::themes::MANIFEST_FILE))?
                .set_modified(std::time::SystemTime::now())?;
        }
    }
    Ok(())
}

/// `themes` ordered so a local parent comes before its children.
fn parents_first(themes: &[LocalTheme]) -> Vec<&LocalTheme> {
    let mut done: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    while out.len() < themes.len() {
        let before = out.len();
        for theme in themes {
            let ready = theme
                .parent
                .as_deref()
                .is_none_or(|p| done.contains(p) || !themes.iter().any(|t| t.name == p));
            if ready && !done.contains(theme.name.as_str()) {
                done.insert(&theme.name);
                out.push(theme);
            }
        }
        if out.len() == before {
            // A cycle: boot reports it properly; just build the rest.
            out.extend(themes.iter().filter(|t| !done.contains(t.name.as_str())));
            break;
        }
    }
    out
}

/// The active theme's name: `configured` (THEME, then `[themes] active`),
/// else the one local theme no other local theme extends.
fn active_theme<'a>(
    themes: &'a [LocalTheme],
    configured: Option<&'a str>,
) -> io::Result<Option<&'a str>> {
    if let Some(name) = configured {
        return Ok(Some(name));
    }
    let leaves: Vec<&str> = themes
        .iter()
        .filter(|t| {
            !themes
                .iter()
                .any(|o| o.parent.as_deref() == Some(t.name.as_str()))
        })
        .map(|t| t.name.as_str())
        .collect();
    match leaves.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one)),
        many => Err(io::Error::other(format!(
            "several themes could be the active one ({}) — set [themes] active in fse.toml",
            many.join(", ")
        ))),
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
    themes: impl Into<crate::themes::ThemeSet>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(name: &str, parent: Option<&str>) -> LocalTheme {
        LocalTheme {
            name: name.to_string(),
            parent: parent.map(str::to_string),
            dir: PathBuf::from(name),
            npm: true,
        }
    }

    #[test]
    fn the_active_theme_is_configured_or_the_only_local_leaf() {
        let themes = [
            theme("dark", Some("app")),
            theme("app", Some("fse-theme-default")),
        ];
        assert_eq!(active_theme(&themes, None).unwrap(), Some("dark"));
        assert_eq!(active_theme(&themes, Some("app")).unwrap(), Some("app"));
        assert_eq!(active_theme(&[], None).unwrap(), None);
        let two = [theme("a", None), theme("b", None)];
        assert!(active_theme(&two, None).is_err());
    }

    #[test]
    fn local_parents_build_first() {
        let themes = [
            theme("a-dark", Some("b-app")),
            theme("b-app", Some("fse-theme-default")),
            theme("c-other", None),
        ];
        let order: Vec<&str> = parents_first(&themes)
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(order, ["b-app", "c-other", "a-dark"]);
    }
}
