//! Test-only helpers for apps built on this framework. Nothing here is used
//! by the framework itself at runtime — pull these into an app's own
//! `tests/` so mistakes that would otherwise only surface as a request-time
//! 500 fail loudly in `cargo test`/CI instead.

use std::fmt::Write as _;

use tera::Tera;

use crate::themes::{Theme, ThemeStack};

/// Resolves `themes` exactly like the app's boot does (active theme = the
/// one no other installed theme extends) and parses every template of the
/// resulting stack into a [`Tera`] — child templates over parent ones, each
/// theme's own copies also under `@{theme}/{name}`. Unlike the boot-time
/// loader, which logs and skips a broken template so one bad page doesn't
/// take the whole app down, this returns every failure.
///
/// Meant for a one-line integration test in a consuming app:
///
/// ```ignore
/// #[test]
/// fn all_templates_parse() {
///     full_stack_engine::testing::load_themes(starter::themes()).unwrap();
/// }
/// ```
///
/// so a broken template (invalid Tera syntax, an `extends` of a missing
/// parent template, an escaping bug in a compile-to-Tera pipeline like
/// `fse-ssr`) fails `cargo test`/CI instead of only surfacing as a runtime
/// 500.
///
/// # Errors
///
/// Returns `Err` when the themes don't resolve (missing parent, ambiguous
/// active theme, …) or with one block per broken template — its name,
/// Tera's error, and its full `source()` chain.
pub fn load_themes(themes: impl IntoIterator<Item = Theme>) -> Result<Tera, String> {
    let stack = theme_stack(themes)?;
    let mut tera = Tera::default();
    tera.autoescape_on(vec![""]);
    let mut errors = Vec::new();
    stack.load_into(&mut tera, &mut |name, err| {
        let mut msg = format!("{name}: {err}");
        let mut source = std::error::Error::source(&err);
        while let Some(cause) = source {
            let _ = write!(msg, "\n  caused by: {cause}");
            source = cause.source();
        }
        errors.push(msg);
    });
    if errors.is_empty() {
        Ok(tera)
    } else {
        Err(errors.join("\n\n"))
    }
}

/// The [`ThemeStack`] `themes` resolve to — for building an `AppData` in
/// tests.
///
/// # Errors
///
/// Returns the resolution error as a string.
pub fn theme_stack(themes: impl IntoIterator<Item = Theme>) -> Result<ThemeStack, String> {
    ThemeStack::resolve(themes.into_iter().collect(), None).map_err(|e| e.to_string())
}
