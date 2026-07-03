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

use include_dir::Dir;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

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
