//! Locale (translation) loading for apps that keep `<lang>.json` files in a
//! directory such as `locales/`. Meant to be called from inside a
//! [`crate::FrameworkApp::global_context_injector`] so every rendered
//! template gets `t` (active locale), `lang` (active code) and `i18n` (every
//! loaded locale, for client-side use) for free.

use std::collections::HashMap;
use std::fs;
use std::sync::{Mutex, OnceLock};

/// Process-lifetime cache of parsed locale directories, keyed by `dir`. Locale
/// files ship inside the binary / image and never change at runtime, so parsing
/// them once and cloning from memory avoids re-reading and re-parsing every
/// file on every request (`inject_locale_context` runs per render).
#[allow(clippy::type_complexity)]
fn locale_cache() -> &'static Mutex<HashMap<String, serde_json::Map<String, serde_json::Value>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, serde_json::Map<String, serde_json::Value>>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Reads and parses every `<lang>.json` file directly inside `dir`, keyed by
/// language code (the file stem). Unreadable or malformed files are skipped.
///
/// The result is cached per `dir` for the lifetime of the process; the first
/// call reads from disk, later calls clone the parsed map from memory.
#[must_use]
pub fn load_all_locales(dir: &str) -> serde_json::Map<String, serde_json::Value> {
    if let Some(cached) = locale_cache().lock().unwrap().get(dir) {
        return cached.clone();
    }

    let mut locales = serde_json::Map::new();

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Some(lang) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(json) = serde_json::from_str(&content) {
                locales.insert(lang.to_string(), json);
            }
        }
    }

    locale_cache()
        .lock()
        .unwrap()
        .insert(dir.to_string(), locales.clone());
    locales
}

/// Reads and parses a single `<dir>/<lang>.json` file, returning `{}` if it's
/// missing or malformed. Handy for one-off lookups (e.g. building an email
/// subject) outside of the global template context.
#[must_use]
pub fn load_locale(dir: &str, lang: &str) -> serde_json::Value {
    fs::read_to_string(format!("{dir}/{lang}.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Inserts `t` (the active locale's translations), `lang` (the active code)
/// and `i18n` (every locale, for client-side use) into a template-context
/// JSON object. Does nothing if `value` isn't a JSON object.
pub fn inject_locale_context(value: &mut serde_json::Value, dir: &str, default_lang: &str) {
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
