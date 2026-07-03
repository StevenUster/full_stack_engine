use full_stack_engine::i18n::{inject_locale_context, load_all_locales, load_locale};
use include_dir::{Dir, include_dir};

static LOCALES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/tests/fixtures/locales");

#[test]
fn loads_locales_and_skips_malformed_files() {
    let locales = load_all_locales(&LOCALES);
    assert!(locales.contains_key("en"));
    assert!(locales.contains_key("de"));
    // `bad.json` is not valid JSON and must be skipped, not crash loading.
    assert!(!locales.contains_key("bad"));
}

#[test]
fn load_locale_returns_translations_or_empty_object() {
    let en = load_locale(&LOCALES, "en");
    assert_eq!(en["greeting"], "Hello");
    assert_eq!(en["nested"]["bye"], "Goodbye");

    // Unknown languages fall back to an empty object instead of erroring.
    assert_eq!(load_locale(&LOCALES, "xx"), serde_json::json!({}));
}

#[test]
fn inject_locale_context_adds_t_lang_and_i18n() {
    let mut ctx = serde_json::json!({ "existing": 1 });
    inject_locale_context(&mut ctx, &LOCALES, "de");

    assert_eq!(ctx["existing"], 1);
    assert_eq!(ctx["lang"], "de");
    assert_eq!(ctx["t"]["greeting"], "Hallo");
    assert_eq!(ctx["i18n"]["en"]["greeting"], "Hello");
}

#[test]
fn inject_locale_context_ignores_non_objects() {
    let mut ctx = serde_json::json!("just a string");
    inject_locale_context(&mut ctx, &LOCALES, "en");
    assert_eq!(ctx, serde_json::json!("just a string"));
}
