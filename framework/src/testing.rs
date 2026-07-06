//! Test-only helpers for apps built on this framework. Nothing here is used
//! by the framework itself at runtime — pull these into an app's own
//! `tests/` so mistakes that would otherwise only surface as a request-time
//! 500 fail loudly in `cargo test`/CI instead.

use std::fmt::Write as _;

use include_dir::Dir;
use tera::Tera;

/// Parses every `.html` file in `dir` into a [`Tera`] instance using the same
/// `index.html` -> `index`, `foo/index.html` -> `foo` naming convention as
/// the app's real boot-time loader. Unlike that loader — which logs and
/// skips a broken template so one bad page doesn't take the whole app down —
/// this collects and returns every parse failure instead of swallowing it.
///
/// Meant for a one-line integration test in a consuming app:
///
/// ```ignore
/// #[test]
/// fn all_templates_parse() {
///     full_stack_engine::testing::load_templates(&DIST_DIR).unwrap();
/// }
/// ```
///
/// so a broken template (invalid Tera syntax, a typo'd variable, an
/// escaping bug in a compile-to-Tera pipeline like `fse-ssr`) fails
/// `cargo test`/CI instead of only surfacing as a runtime 500.
///
/// # Errors
///
/// Returns `Err` with one block per broken template — its name, Tera's
/// error, and its full `source()` chain — if any template fails to parse.
pub fn load_templates(dir: &Dir) -> Result<Tera, String> {
    let mut tera = Tera::default();
    tera.autoescape_on(vec![""]);
    let mut errors = Vec::new();
    crate::walk_templates(&mut tera, dir, &mut |name, err| {
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
