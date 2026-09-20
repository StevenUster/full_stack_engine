//! Text transforms every app ends up needing: URL-safe slugs and
//! link-stripping for public free text.
//!
//! These live in the framework rather than in each app because both were
//! hand-rolled in application code before, and both are the kind of function
//! whose edge cases (non-ASCII input, scheme-less URLs) are discovered once
//! and then quietly re-broken in the next copy.
//!
//! For escaping HTML, use [`tera::escape_html`] — the prelude already
//! re-exports `tera`, so there is no reason for an app to write its own
//! `&`/`<`/`>` replacement chain.

/// Turns a title into a URL-safe slug: lowercase, alphanumerics kept, every
/// run of anything else collapsed to a single `-`.
///
/// Non-ASCII letters are kept (`Straße` → `straße`), because dropping them
/// silently turns a German or Turkish title into an empty slug. Transliteration
/// is deliberately not attempted: it needs a language to be correct, and a
/// wrong guess is worse than a faithful one.
///
/// ```
/// # use full_stack_engine::text::slugify;
/// assert_eq!(slugify("Charity Run München 2026!"), "charity-run-münchen-2026");
/// assert_eq!(slugify("  --Hello---World--  "), "hello-world");
/// assert_eq!(slugify("!!!"), "");
/// ```
#[must_use]
pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pending_dash = false;
    for ch in input.chars() {
        if ch.is_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.extend(ch.to_lowercase());
        } else {
            pending_dash = true;
        }
    }
    out
}

/// Strips every URL from user-supplied free text, keeping line breaks and
/// collapsing the whitespace the removal leaves behind.
///
/// For text that is shown to *other* users — comments, donor messages,
/// profile blurbs. Escaping a link renders it harmless to the browser but
/// still displays it, which is exactly what spam wants; this moderates it out
/// instead.
///
/// Scheme-less hosts (`www.example.com`, `example.de/path`) are matched too,
/// since those are what spam actually uses. Abbreviations that merely look
/// host-like (`z.B.`) are left alone.
///
/// ```
/// # use full_stack_engine::text::strip_urls;
/// assert_eq!(strip_urls("Great cause! https://evil.example/x keep going"), "Great cause! keep going");
/// assert_eq!(strip_urls("visit www.example.com now"), "visit now");
/// assert_eq!(strip_urls("z.B. schöne Sache"), "z.B. schöne Sache");
/// ```
#[must_use]
pub fn strip_urls(text: &str) -> String {
    use linkify::{LinkFinder, LinkKind};

    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]);
    finder.url_must_have_scheme(false);

    let mut stripped = String::with_capacity(text.len());
    let mut last_end = 0;
    for link in finder.links(text) {
        stripped.push_str(&text[last_end..link.start()]);
        last_end = link.end();
    }
    stripped.push_str(&text[last_end..]);

    stripped
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_collapses_separators_and_keeps_non_ascii() {
        assert_eq!(
            slugify("Charity Run München 2026!"),
            "charity-run-münchen-2026"
        );
        assert_eq!(slugify("  --Hello---World--  "), "hello-world");
        assert_eq!(slugify("A/B & C"), "a-b-c");
        // Nothing alphanumeric at all: an empty slug, not a string of dashes.
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify(""), "");
        // A slug never starts or ends with the separator, whatever the input.
        for input in ["-x-", "...x...", " x "] {
            let slug = slugify(input);
            assert!(!slug.starts_with('-') && !slug.ends_with('-'), "{slug:?}");
        }
    }

    #[test]
    fn strip_urls_removes_links_with_and_without_scheme() {
        assert_eq!(
            strip_urls("Great cause! https://evil.example/phish keep it up"),
            "Great cause! keep it up"
        );
        assert_eq!(strip_urls("visit www.example.com now"), "visit now");
        assert_eq!(strip_urls("check example.de/path please"), "check please");
        // A message that is nothing but a link becomes empty, which the caller
        // can then reject.
        assert_eq!(strip_urls("https://spam.example"), "");
    }

    #[test]
    fn strip_urls_keeps_line_breaks_and_ordinary_abbreviations() {
        assert_eq!(
            strip_urls("Toi toi toi!\nViel Erfolg beim Lauf."),
            "Toi toi toi!\nViel Erfolg beim Lauf."
        );
        assert_eq!(strip_urls("z.B. schöne Sache"), "z.B. schöne Sache");
    }
}
