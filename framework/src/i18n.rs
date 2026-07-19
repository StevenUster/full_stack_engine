//! Locale (translation) loading for apps that keep `<lang>.json` files in a
//! directory such as `locales/`.
//!
//! The directory is embedded into the binary at compile time with
//! [`include_dir!`](include_dir::include_dir), so a built app has no external
//! locale files to ship — everything lives inside the executable. Meant to be
//! called from inside a [`crate::FrameworkApp::global_context_injector`] so
//! every rendered template gets `t` (active locale), `lang` (active code) and
//! `i18n` (every loaded locale, for client-side use) for free.
//!
//! ```ignore
//! use full_stack_engine::include_dir::{Dir, include_dir};
//! static LOCALES: Dir = include_dir!("$CARGO_MANIFEST_DIR/locales");
//! // ...
//! inject_locale_context(value, &LOCALES, "en");
//! ```

use actix_web::HttpMessage as _;
use include_dir::Dir;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// The framework's own translations for everything the generated CRUD UI
/// says (`model_ui.*`, `form_errors.*`). Apps and modules layer on top; an
/// app key always wins.
static FRAMEWORK_LOCALES: &[(&str, &str)] = &[
    ("en", include_str!("../locales/en.json")),
    ("de", include_str!("../locales/de.json")),
];

/// How the active language of a request is decided. Exactly one strategy is
/// in effect (see [`crate::FrameworkApp::locales`]); apps that never call it
/// get `Hardcoded("en")`.
#[derive(Debug, Clone)]
pub enum LocaleSelector {
    /// One fixed language for the whole app.
    Hardcoded(String),
    /// The request's `Host` decides: `[("example.de", "de"), ("de.example.com", "de")]`.
    /// Unmapped hosts get `default`.
    Domain {
        map: Vec<(String, String)>,
        default: String,
    },
    /// A leading path segment decides: `/de/products` is German, `/products`
    /// is the (unprefixed) default language.
    Path { default: String },
}

impl Default for LocaleSelector {
    fn default() -> Self {
        LocaleSelector::Hardcoded("en".to_string())
    }
}

impl LocaleSelector {
    #[must_use]
    pub fn default_lang(&self) -> &str {
        match self {
            LocaleSelector::Hardcoded(lang) => lang,
            LocaleSelector::Domain { default, .. } | LocaleSelector::Path { default } => default,
        }
    }
}

/// Request extension: the language resolved by [`apply_request_locale`].
#[derive(Debug, Clone)]
pub struct RequestLang(pub String);

/// Deep-merges `overlay` into `base`: objects merge recursively, everything
/// else in `overlay` replaces the `base` value.
pub fn merge_locale(base: &mut serde_json::Value, overlay: &serde_json::Value) {
    if let (Some(base_obj), Some(overlay_obj)) = (base.as_object_mut(), overlay.as_object()) {
        for (key, value) in overlay_obj {
            match base_obj.get_mut(key) {
                Some(existing) if existing.is_object() && value.is_object() => {
                    merge_locale(existing, value);
                }
                _ => {
                    base_obj.insert(key.clone(), value.clone());
                }
            }
        }
    } else {
        *base = overlay.clone();
    }
}

/// The full locale set an app serves: the framework's base translations with
/// the app's files layered on top (per language, deep-merged, app wins). App
/// languages the framework doesn't know start from the framework's default
/// so generated-UI chrome never goes missing entirely.
///
/// # Panics
///
/// Never in practice: only if the framework's own embedded locale files were
/// invalid JSON, which the build would already have caught in tests.
#[must_use]
pub fn build_locales(app_dir: Option<&Dir>) -> HashMap<String, serde_json::Value> {
    let mut locales: HashMap<String, serde_json::Value> = FRAMEWORK_LOCALES
        .iter()
        .map(|(lang, raw)| {
            (
                (*lang).to_string(),
                serde_json::from_str(raw).expect("framework locale files are valid JSON"),
            )
        })
        .collect();

    if let Some(dir) = app_dir {
        for (lang, overlay) in load_all_locales(dir) {
            let base = locales
                .entry(lang.clone())
                .or_insert_with(|| serde_json::json!({}));
            merge_locale(base, &overlay);
        }
    }
    locales
}

/// Makes every language self-sufficient: each non-default locale becomes
/// "default language deep-merged with its own keys", so a partially
/// translated language falls back per key instead of rendering holes.
// A concrete HashMap in and out: this is plumbing between build_locales and
// AppData, not a generic API surface.
#[allow(clippy::implicit_hasher)]
#[must_use]
pub fn resolve_locales(
    mut locales: HashMap<String, serde_json::Value>,
    default_lang: &str,
) -> HashMap<String, serde_json::Value> {
    let default = locales
        .get(default_lang)
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    for (lang, value) in &mut locales {
        if lang != default_lang {
            let mut merged = default.clone();
            merge_locale(&mut merged, value);
            *value = merged;
        }
    }
    locales
}

/// Resolves the request's language per `selector` and stores it as a
/// [`RequestLang`] extension. In `Path` mode a recognized `/{lang}` prefix
/// is stripped from the URI before routing, so routes are registered once,
/// unprefixed. Shared by the framework's middleware and by tests.
pub fn apply_request_locale(
    selector: &LocaleSelector,
    known_langs: &[String],
    req: &mut actix_web::dev::ServiceRequest,
) {
    let lang = match selector {
        LocaleSelector::Hardcoded(lang) => lang.clone(),
        LocaleSelector::Domain { map, default } => {
            let info = req.connection_info().clone();
            let host = info.host().split(':').next().unwrap_or("");
            map.iter()
                .find(|(h, _)| h.eq_ignore_ascii_case(host))
                .map_or_else(|| default.clone(), |(_, lang)| lang.clone())
        }
        LocaleSelector::Path { default } => {
            match split_lang_prefix(req.path(), known_langs, default) {
                Some((lang, rest)) => {
                    rewrite_path(req, &rest);
                    lang
                }
                None => default.clone(),
            }
        }
    };
    req.extensions_mut().insert(RequestLang(lang));
}

/// `/de/products` → `("de", "/products")` when `de` is a known non-default
/// language; `None` when the path carries no language prefix. The default
/// language is never treated as a prefix — it lives unprefixed.
fn split_lang_prefix(path: &str, known_langs: &[String], default: &str) -> Option<(String, String)> {
    let trimmed = path.strip_prefix('/')?;
    let (seg, rest) = trimmed
        .split_once('/')
        .map_or((trimmed, ""), |(a, b)| (a, b));
    if seg == default || !known_langs.iter().any(|l| l == seg) {
        return None;
    }
    Some((seg.to_string(), format!("/{rest}")))
}

fn rewrite_path(req: &mut actix_web::dev::ServiceRequest, new_path: &str) {
    let uri = req.head().uri.clone();
    let path_and_query = match uri.query() {
        Some(q) => format!("{new_path}?{q}"),
        None => new_path.to_string(),
    };
    let mut parts = uri.into_parts();
    if let Ok(pq) = path_and_query.parse() {
        parts.path_and_query = Some(pq);
        if let Ok(new_uri) = actix_web::http::Uri::from_parts(parts) {
            req.head_mut().uri = new_uri.clone();
            // Routing matches against match_info's copy of the URL, captured
            // before middleware runs — update it too (as NormalizePath does).
            req.match_info_mut().get_mut().update(&new_uri);
        }
    }
}

/// Process-lifetime cache of parsed locale directories, keyed by the address of
/// the embedded [`Dir`] static. The files are baked into the binary and never
/// change at runtime, so parsing them once and cloning from memory avoids
/// re-parsing every locale on every request (`inject_locale_context` runs per
/// render).
fn locale_cache() -> &'static Mutex<HashMap<usize, serde_json::Map<String, serde_json::Value>>> {
    static CACHE: OnceLock<Mutex<HashMap<usize, serde_json::Map<String, serde_json::Value>>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Parses every top-level `<lang>.json` file in the embedded `dir`, keyed by
/// language code (the file stem). Malformed or non-UTF-8 files are skipped.
///
/// The result is cached for the lifetime of the process; the first call parses
/// the embedded bytes, later calls clone the parsed map from memory.
///
/// # Panics
///
/// Panics if the internal cache mutex is poisoned (a previous panic while
/// holding it).
#[must_use]
pub fn load_all_locales(dir: &Dir) -> serde_json::Map<String, serde_json::Value> {
    let key = std::ptr::from_ref(dir) as usize;
    if let Some(cached) = locale_cache().lock().unwrap().get(&key) {
        return cached.clone();
    }

    let mut locales = serde_json::Map::new();

    for file in dir.files() {
        let path = file.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Some(lang) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(content) = file.contents_utf8() else {
            continue;
        };
        if let Ok(json) = serde_json::from_str(content) {
            locales.insert(lang.to_string(), json);
        }
    }

    locale_cache().lock().unwrap().insert(key, locales.clone());
    locales
}

/// Parses a single embedded `<lang>.json` file, returning `{}` if it's missing
/// or malformed. Handy for one-off lookups (e.g. building an email subject)
/// outside of the global template context.
#[must_use]
pub fn load_locale(dir: &Dir, lang: &str) -> serde_json::Value {
    load_all_locales(dir)
        .get(lang)
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Inserts `t` (the active locale's translations), `lang` (the active code)
/// and `i18n` (every locale, for client-side use) into a template-context
/// JSON object. Does nothing if `value` isn't a JSON object.
pub fn inject_locale_context(value: &mut serde_json::Value, dir: &Dir, default_lang: &str) {
    let locales = load_all_locales(dir);

    let Some(obj) = value.as_object_mut() else {
        return;
    };

    if let Some(t) = locales.get(default_lang) {
        obj.insert("t".to_string(), t.clone());
    }
    obj.insert("lang".to_string(), serde_json::json!(default_lang));
    obj.insert("i18n".to_string(), serde_json::Value::Object(locales));
}
