//! Themes — WordPress-style parent/child layering of *built* frontends.
//!
//! A theme is the output of any HTML-generating tool (Astro through the
//! `fse-ssr` integration, Eleventy, a hand-written folder, …): one directory
//! containing
//!
//! - `theme.json` — the manifest: `{ "name": "...", "parent": "..." }`,
//! - `**/*.html` — Tera templates, named by path (`index.html` → `index`,
//!   `login/index.html` or `login.html` → `login`),
//! - everything else — static assets served at the same URL path
//!   (`_astro/app.css` → `/_astro/app.css`).
//!
//! An app installs any number of themes and activates one. The active theme
//! plus its ancestors (following `parent`) form the [`ThemeStack`]; lookups
//! walk it child-first, so a child theme only needs to contain what it
//! changes — every template or asset it doesn't have falls back to its
//! parent, then its grandparent. Each theme's templates are additionally
//! registered under `@{theme-name}/{template}`, so a child template can
//! `{% extends "@fse-theme-default/login" %}` or include a parent part
//! it overrides.
//!
//! ```ignore
//! static THEME: Dir = include_dir!("$CARGO_MANIFEST_DIR/theme/dist");
//!
//! FrameworkApp::new()
//!     .theme(Theme::embedded(&fse_theme_default::DIST)) // parent (cargo crate)
//!     .theme(Theme::embedded(&THEME).dev_server("http://localhost:4321"))
//! ```

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use include_dir::Dir;
use log::debug;
use serde::{Deserialize, Serialize};
use tera::Tera;

/// The manifest file every built theme carries at its root.
pub const MANIFEST_FILE: &str = "theme.json";

/// Contents of a theme's `theme.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeManifest {
    /// Unique theme name — what child themes reference as `parent`.
    pub name: String,
    /// Name of the theme this one extends, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("theme has no {MANIFEST_FILE} at its root{0}")]
    MissingManifest(String),
    #[error("invalid {MANIFEST_FILE}{0}: {1}")]
    InvalidManifest(String, serde_json::Error),
    #[error("failed to read theme directory {0}: {1}")]
    Io(String, std::io::Error),
    #[error("theme \"{0}\" is installed twice")]
    Duplicate(String),
    #[error("no theme is installed")]
    NoThemes,
    #[error("active theme \"{0}\" is not installed (installed: {1})")]
    UnknownActive(String, String),
    #[error(
        "several installed themes could be the active one ({0}) — pick one with \
         FrameworkApp::active_theme or the THEME environment variable"
    )]
    AmbiguousActive(String),
    #[error("theme \"{child}\" extends \"{parent}\", which is not installed")]
    MissingParent { child: String, parent: String },
    #[error("theme inheritance cycle: {0}")]
    Cycle(String),
}

/// One installed theme: its manifest and every file of its built output.
#[derive(Clone)]
pub struct Theme {
    manifest: ThemeManifest,
    /// Root-relative, `/`-separated paths.
    files: BTreeMap<String, Cow<'static, [u8]>>,
    dev_server: Option<String>,
}

impl std::fmt::Debug for Theme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Theme")
            .field("manifest", &self.manifest)
            .field("files", &self.files.len())
            .field("dev_server", &self.dev_server)
            .finish()
    }
}

impl Theme {
    /// A theme embedded in the binary with `include_dir!` — a local folder
    /// or a theme crate (e.g. `fse_theme_default::DIST`).
    ///
    /// # Panics
    ///
    /// Panics when the directory has no valid `theme.json`: an embedded theme
    /// is fixed at compile time, so this is a build mistake to fail loudly
    /// on at boot (use [`Theme::try_embedded`] to handle it instead).
    #[must_use]
    pub fn embedded(dir: &'static Dir<'static>) -> Self {
        Self::try_embedded(dir).unwrap_or_else(|err| panic!("{err}"))
    }

    /// Fallible [`Theme::embedded`].
    ///
    /// # Errors
    ///
    /// Returns [`ThemeError`] when `theme.json` is missing or invalid.
    pub fn try_embedded(dir: &'static Dir<'static>) -> Result<Self, ThemeError> {
        let mut files = BTreeMap::new();
        collect_embedded(dir, &mut files);
        Self::from_files(files, "")
    }

    /// A theme read from disk at boot — drop-in themes that are deployed
    /// next to the binary instead of compiled into it.
    ///
    /// # Errors
    ///
    /// Returns [`ThemeError`] when the directory can't be read or has no
    /// valid `theme.json`.
    pub fn from_directory(path: impl AsRef<Path>) -> Result<Self, ThemeError> {
        let root = path.as_ref();
        let mut files = BTreeMap::new();
        collect_disk(root, root, &mut files)
            .map_err(|e| ThemeError::Io(root.display().to_string(), e))?;
        Self::from_files(files, &format!(" ({})", root.display()))
    }

    /// A theme assembled in code — mostly for tests and generated themes.
    #[must_use]
    pub fn new(manifest: ThemeManifest) -> Self {
        Self {
            manifest,
            files: BTreeMap::new(),
            dev_server: None,
        }
    }

    /// Adds (or replaces) one file of a [`Theme::new`] theme.
    #[must_use]
    pub fn with_file(mut self, path: &str, contents: impl Into<Vec<u8>>) -> Self {
        self.files
            .insert(normalize(path), Cow::Owned(contents.into()));
        self
    }

    /// In `ENV=dev`, pages and assets are fetched from this dev server first
    /// (e.g. `astro dev` on `http://localhost:4321`), falling back to the
    /// built files when it doesn't serve them.
    #[must_use]
    pub fn dev_server(mut self, url: impl Into<String>) -> Self {
        self.dev_server = Some(url.into().trim_end_matches('/').to_string());
        self
    }

    fn from_files(
        files: BTreeMap<String, Cow<'static, [u8]>>,
        location: &str,
    ) -> Result<Self, ThemeError> {
        let raw = files
            .get(MANIFEST_FILE)
            .ok_or_else(|| ThemeError::MissingManifest(location.to_string()))?;
        let manifest: ThemeManifest = serde_json::from_slice(raw)
            .map_err(|e| ThemeError::InvalidManifest(location.to_string(), e))?;
        Ok(Self {
            manifest,
            files,
            dev_server: None,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    #[must_use]
    pub fn manifest(&self) -> &ThemeManifest {
        &self.manifest
    }

    #[must_use]
    pub fn dev_server_url(&self) -> Option<&str> {
        self.dev_server.as_deref()
    }

    /// The raw bytes of one file, by root-relative path.
    #[must_use]
    pub fn file(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(AsRef::as_ref)
    }

    /// Every template of this theme as `(name, source)`.
    pub fn templates(&self) -> impl Iterator<Item = (String, &str)> {
        self.files.iter().filter_map(|(path, bytes)| {
            let name = template_name(path)?;
            if let Ok(source) = std::str::from_utf8(bytes) {
                Some((name, source))
            } else {
                log::error!(
                    "Skipping template with non-UTF-8 contents: {}/{name}",
                    self.name()
                );
                None
            }
        })
    }

    /// Whether this theme itself (not an ancestor) has the template.
    #[must_use]
    pub fn has_template(&self, name: &str) -> bool {
        template_paths(name)
            .iter()
            .any(|p| self.files.contains_key(p))
    }
}

/// The active theme and its ancestors, child first.
#[derive(Debug, Clone, Default)]
pub struct ThemeStack {
    chain: Vec<Theme>,
}

impl ThemeStack {
    /// Picks the active theme among `installed` and follows `parent` links.
    ///
    /// `active` names the theme to activate. When `None`, the one installed
    /// theme that no other installed theme extends is used — so installing a
    /// parent and its child activates the child without further setup.
    /// Installed themes outside the active chain are ignored (like
    /// `WordPress`'s inactive themes).
    ///
    /// # Errors
    ///
    /// Returns [`ThemeError`] on duplicate names, an unknown/ambiguous active
    /// theme, a missing parent or an inheritance cycle.
    pub fn resolve(installed: Vec<Theme>, active: Option<&str>) -> Result<Self, ThemeError> {
        if installed.is_empty() {
            return Err(ThemeError::NoThemes);
        }
        let mut by_name: HashMap<String, Theme> = HashMap::new();
        let mut order = Vec::new();
        for theme in installed {
            let name = theme.name().to_string();
            if by_name.contains_key(&name) {
                return Err(ThemeError::Duplicate(name));
            }
            order.push(name.clone());
            by_name.insert(name, theme);
        }

        let active = if let Some(active) = active {
            if !by_name.contains_key(active) {
                return Err(ThemeError::UnknownActive(
                    active.to_string(),
                    order.join(", "),
                ));
            }
            active.to_string()
        } else {
            let extended: HashSet<&str> = by_name
                .values()
                .filter_map(|t| t.manifest.parent.as_deref())
                .collect();
            let leaves: Vec<&String> = order
                .iter()
                .filter(|name| !extended.contains(name.as_str()))
                .collect();
            match leaves.as_slice() {
                [one] => (*one).clone(),
                [] => return Err(ThemeError::Cycle(order.join(" -> "))),
                many => {
                    let names: Vec<&str> = many.iter().map(|s| s.as_str()).collect();
                    return Err(ThemeError::AmbiguousActive(names.join(", ")));
                }
            }
        };

        let mut chain = Vec::new();
        let mut seen = Vec::new();
        let mut next = Some(active);
        while let Some(name) = next.take() {
            if seen.contains(&name) {
                seen.push(name);
                return Err(ThemeError::Cycle(seen.join(" -> ")));
            }
            let theme = by_name
                .remove(&name)
                .ok_or_else(|| ThemeError::MissingParent {
                    child: seen.last().cloned().unwrap_or_default(),
                    parent: name.clone(),
                })?;
            next.clone_from(&theme.manifest.parent);
            seen.push(name);
            chain.push(theme);
        }
        Ok(Self { chain })
    }

    /// Active theme first, root ancestor last.
    #[must_use]
    pub fn chain(&self) -> &[Theme] {
        &self.chain
    }

    #[must_use]
    pub fn active(&self) -> Option<&Theme> {
        self.chain.first()
    }

    /// The most specific theme's copy of a static asset. Templates (their
    /// unrendered source) and the manifest are not assets.
    #[must_use]
    pub fn asset(&self, path: &str) -> Option<&[u8]> {
        let path = path.trim_start_matches('/');
        if path == MANIFEST_FILE || template_name(path).is_some() {
            return None;
        }
        self.chain.iter().find_map(|t| t.file(path))
    }

    /// The theme that provides template `name` (child first).
    #[must_use]
    pub fn template_owner(&self, name: &str) -> Option<&Theme> {
        self.chain.iter().find(|t| t.has_template(name))
    }

    /// Every template of the stack, resolved: plain names map to the most
    /// specific theme's source, and every theme's own copy is also available
    /// as `@{theme}/{name}`.
    #[must_use]
    pub fn template_sources(&self) -> BTreeMap<String, &str> {
        let mut out = BTreeMap::new();
        // Root ancestor first, so more specific themes overwrite.
        for theme in self.chain.iter().rev() {
            for (name, source) in theme.templates() {
                out.insert(format!("@{}/{name}", theme.name()), source);
                out.insert(name, source);
            }
        }
        out
    }

    /// Loads [`ThemeStack::template_sources`] into `tera`. A template that
    /// fails to parse (or extends a template that doesn't exist) is reported
    /// through `on_error` and left out; everything else still loads.
    pub fn load_into(&self, tera: &mut Tera, on_error: &mut dyn FnMut(&str, tera::Error)) {
        let sources = self.template_sources();
        for name in sources.keys() {
            debug!("Registering template: {name}");
        }
        // Fast path: one batch, so `extends`/`include` resolve regardless of
        // order.
        if tera
            .add_raw_templates(sources.iter().map(|(n, s)| (n.as_str(), *s)))
            .is_ok()
        {
            return;
        }
        // Something is broken, and a failed batch leaves Tera half-filled.
        // Start over: drop templates that don't parse, then (repeatedly)
        // templates extending one that isn't there, and load the rest.
        let autoescape = tera.autoescape_suffixes.clone();
        *tera = Tera::default();
        tera.autoescape_on(autoescape);

        let mut parsed: BTreeMap<&str, (&str, Option<String>)> = BTreeMap::new();
        for (name, source) in &sources {
            match tera::Template::new(name, None, source) {
                Ok(tpl) => {
                    parsed.insert(name.as_str(), (*source, tpl.parent));
                }
                Err(err) => on_error(name, err),
            }
        }
        loop {
            let orphan = parsed.iter().find_map(|(name, (_, parent))| {
                parent
                    .as_deref()
                    .filter(|p| !parsed.contains_key(p))
                    .map(|p| (*name, p.to_string()))
            });
            let Some((name, parent)) = orphan else { break };
            parsed.remove(name);
            on_error(
                name,
                tera::Error::msg(format!("extends \"{parent}\", which is missing or broken")),
            );
        }
        if let Err(err) = tera.add_raw_templates(parsed.iter().map(|(n, (s, _))| (*n, *s))) {
            on_error("(theme templates)", err);
        }
    }

    /// A fresh autoescaping [`Tera`] with every template of the stack; broken
    /// templates are logged and skipped so one bad page can't take the app
    /// down.
    #[must_use]
    pub fn tera(&self) -> Tera {
        let mut tera = Tera::default();
        tera.autoescape_on(vec![""]);
        self.load_into(&mut tera, &mut |name, err| {
            log::error!("Skipping invalid template {name}: {err}");
        });
        tera
    }

    /// Dev servers of the stack, child first.
    pub fn dev_servers(&self) -> impl Iterator<Item = &str> {
        self.chain.iter().filter_map(Theme::dev_server_url)
    }
}

/// `index.html` → `index`, `a/index.html` → `a`, `a/b.html` → `a/b`;
/// non-HTML files are not templates.
#[must_use]
pub fn template_name(path: &str) -> Option<String> {
    let stem = path.strip_suffix(".html")?;
    if stem == "index" {
        return Some("index".to_string());
    }
    Some(stem.strip_suffix("/index").unwrap_or(stem).to_string())
}

/// The file paths a template name may be stored under.
fn template_paths(name: &str) -> [String; 2] {
    if name == "index" {
        ["index.html".to_string(), "index.html".to_string()]
    } else {
        [format!("{name}.html"), format!("{name}/index.html")]
    }
}

fn normalize(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches('/').to_string()
}

fn collect_embedded(dir: &'static Dir<'static>, out: &mut BTreeMap<String, Cow<'static, [u8]>>) {
    for file in dir.files() {
        if let Some(path) = file.path().to_str() {
            out.insert(normalize(path), Cow::Borrowed(file.contents()));
        } else {
            log::error!(
                "Skipping theme file with non-UTF-8 path: {}",
                file.path().display()
            );
        }
    }
    for sub in dir.dirs() {
        collect_embedded(sub, out);
    }
}

fn collect_disk(
    root: &Path,
    dir: &Path,
    out: &mut BTreeMap<String, Cow<'static, [u8]>>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_disk(root, &path, out)?;
        } else if let Some(rel) = path.strip_prefix(root).ok().and_then(Path::to_str) {
            out.insert(normalize(rel), Cow::Owned(std::fs::read(&path)?));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(name: &str, parent: Option<&str>) -> Theme {
        Theme::new(ThemeManifest {
            name: name.to_string(),
            parent: parent.map(str::to_string),
            version: None,
            description: None,
        })
    }

    #[test]
    fn template_names_follow_the_path_convention() {
        assert_eq!(template_name("index.html").as_deref(), Some("index"));
        assert_eq!(template_name("login/index.html").as_deref(), Some("login"));
        assert_eq!(
            template_name("emails/verify.html").as_deref(),
            Some("emails/verify")
        );
        assert_eq!(template_name("_astro/app.css"), None);
    }

    #[test]
    fn child_is_active_by_default_and_chain_follows_parents() {
        let stack = ThemeStack::resolve(
            vec![
                theme("base", None),
                theme("child", Some("mid")),
                theme("mid", Some("base")),
            ],
            None,
        )
        .unwrap();
        let names: Vec<&str> = stack.chain().iter().map(Theme::name).collect();
        assert_eq!(names, ["child", "mid", "base"]);
    }

    #[test]
    fn explicit_active_theme_ignores_unrelated_themes() {
        let stack =
            ThemeStack::resolve(vec![theme("a", None), theme("b", None)], Some("b")).unwrap();
        assert_eq!(stack.chain().len(), 1);
        assert_eq!(stack.active().unwrap().name(), "b");
    }

    #[test]
    fn resolution_errors_are_reported() {
        assert!(matches!(
            ThemeStack::resolve(vec![theme("a", None), theme("b", None)], None),
            Err(ThemeError::AmbiguousActive(_))
        ));
        assert!(matches!(
            ThemeStack::resolve(vec![theme("a", Some("nope"))], None),
            Err(ThemeError::MissingParent { .. })
        ));
        assert!(matches!(
            ThemeStack::resolve(vec![theme("a", None), theme("a", None)], None),
            Err(ThemeError::Duplicate(_))
        ));
        assert!(matches!(
            ThemeStack::resolve(
                vec![theme("a", Some("b")), theme("b", Some("a"))],
                Some("a")
            ),
            Err(ThemeError::Cycle(_))
        ));
        assert!(matches!(
            ThemeStack::resolve(vec![theme("a", None)], Some("x")),
            Err(ThemeError::UnknownActive(..))
        ));
    }

    #[test]
    fn templates_and_assets_fall_back_to_the_parent() {
        let parent = theme("base", None)
            .with_file("login/index.html", "BASE LOGIN")
            .with_file("index.html", "BASE HOME")
            .with_file("_astro/app.css", "base-css")
            .with_file("theme.json", "{}");
        let child = theme("child", Some("base"))
            .with_file("index.html", "CHILD HOME {% include \"@base/index\" %}")
            .with_file("custom.css", "child-css");
        let stack = ThemeStack::resolve(vec![parent, child], None).unwrap();

        let tera = stack.tera();
        let ctx = tera::Context::new();
        assert_eq!(tera.render("login", &ctx).unwrap(), "BASE LOGIN");
        assert_eq!(tera.render("index", &ctx).unwrap(), "CHILD HOME BASE HOME");
        assert_eq!(stack.asset("/_astro/app.css"), Some(&b"base-css"[..]));
        assert_eq!(stack.asset("custom.css"), Some(&b"child-css"[..]));
        assert_eq!(stack.asset("theme.json"), None);
        assert_eq!(stack.asset("index.html"), None);
        assert_eq!(stack.template_owner("login").unwrap().name(), "base");
        assert_eq!(stack.template_owner("index").unwrap().name(), "child");
    }

    #[test]
    fn child_templates_can_extend_parent_templates() {
        let parent = theme("base", None).with_file(
            "layout.html",
            "<main>{% block body %}base{% endblock %}</main>",
        );
        let child = theme("child", Some("base")).with_file(
            "page.html",
            "{% extends \"@base/layout\" %}{% block body %}child{% endblock %}",
        );
        let stack = ThemeStack::resolve(vec![child, parent], None).unwrap();
        let html = stack.tera().render("page", &tera::Context::new()).unwrap();
        assert_eq!(html, "<main>child</main>");
    }

    #[test]
    fn a_broken_template_is_skipped_without_losing_the_rest() {
        let parent = theme("base", None)
            .with_file("layout.html", "{% block body %}{% endblock %}")
            .with_file("broken.html", "{% if %}");
        let child = theme("child", Some("base")).with_file(
            "page.html",
            "{% extends \"layout\" %}{% block body %}ok{% endblock %}",
        );
        let stack = ThemeStack::resolve(vec![parent, child], None).unwrap();
        let mut errors = Vec::new();
        let mut tera = Tera::default();
        stack.load_into(&mut tera, &mut |name, _| errors.push(name.to_string()));
        assert!(errors.contains(&"broken".to_string()));
        assert_eq!(tera.render("page", &tera::Context::new()).unwrap(), "ok");
    }

    #[test]
    fn directory_themes_read_their_manifest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("theme.json"),
            r#"{ "name": "disk", "parent": "base" }"#,
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("login")).unwrap();
        std::fs::write(dir.path().join("login/index.html"), "DISK").unwrap();
        let t = Theme::from_directory(dir.path()).unwrap();
        assert_eq!(t.name(), "disk");
        assert_eq!(t.manifest().parent.as_deref(), Some("base"));
        assert!(t.has_template("login"));

        let empty = tempfile::tempdir().unwrap();
        assert!(matches!(
            Theme::from_directory(empty.path()),
            Err(ThemeError::MissingManifest(_))
        ));
    }
}
