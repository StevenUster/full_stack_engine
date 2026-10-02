//! Form-field parsing helpers the generated `create`/`update` code calls.
//!
//! Every helper returns `Option<T>`: `None` means "invalid, an error was
//! recorded" — the generated code parses all fields first, collects every
//! error for the re-rendered form, and only unwraps once the error list is
//! empty. Missing and empty-string fields are equivalent (browsers submit
//! empty inputs as empty strings).

use super::resource::{FieldError, FormData, FormErrors};
use chrono::NaiveDateTime;

fn raw<'a>(form: &'a FormData, name: &str) -> Option<&'a str> {
    form.get(name).map(|v| v.trim()).filter(|v| !v.is_empty())
}

fn err(errors: &mut FormErrors, field: &'static str, code: &'static str) {
    errors.push(FieldError { field, code });
}

/// A required text column: empty → `required`.
pub fn req_str(form: &FormData, name: &'static str, errors: &mut FormErrors) -> Option<String> {
    if let Some(v) = raw(form, name) {
        Some(v.to_string())
    } else {
        err(errors, name, "required");
        None
    }
}

/// A nullable text column: empty → `NULL`. Cannot fail.
#[must_use]
pub fn opt_str(form: &FormData, name: &str) -> Option<String> {
    raw(form, name).map(str::to_string)
}

/// A required `FromStr` column (numbers, enums, uuids, dates); `code` is the
/// error recorded for unparseable input.
pub fn req_parse<T: std::str::FromStr>(
    form: &FormData,
    name: &'static str,
    code: &'static str,
    errors: &mut FormErrors,
) -> Option<T> {
    match raw(form, name) {
        None => {
            err(errors, name, "required");
            None
        }
        Some(v) => {
            if let Ok(parsed) = v.parse() {
                Some(parsed)
            } else {
                err(errors, name, code);
                None
            }
        }
    }
}

/// A nullable `FromStr` column: empty → `Some(None)`, unparseable → `None` +
/// error.
pub fn opt_parse<T: std::str::FromStr>(
    form: &FormData,
    name: &'static str,
    code: &'static str,
    errors: &mut FormErrors,
) -> Option<Option<T>> {
    match raw(form, name) {
        None => Some(None),
        Some(v) => {
            if let Ok(parsed) = v.parse() {
                Some(Some(parsed))
            } else {
                err(errors, name, code);
                None
            }
        }
    }
}

/// A required decimal column: `1.5` or `1,5` (what a German keyboard
/// types); unparseable → `invalid_number`.
pub fn req_decimal(form: &FormData, name: &'static str, errors: &mut FormErrors) -> Option<f64> {
    let Some(v) = raw(form, name) else {
        err(errors, name, "required");
        return None;
    };
    let parsed = crate::forms::parse_decimal(v);
    if parsed.is_none() {
        err(errors, name, "invalid_number");
    }
    parsed
}

/// A nullable decimal column: empty → `NULL`, else as [`req_decimal`].
pub fn opt_decimal(
    form: &FormData,
    name: &'static str,
    errors: &mut FormErrors,
) -> Option<Option<f64>> {
    match raw(form, name) {
        None => Some(None),
        Some(v) => {
            if let Some(n) = crate::forms::parse_decimal(v) {
                Some(Some(n))
            } else {
                err(errors, name, "invalid_number");
                None
            }
        }
    }
}

/// An HTML checkbox: present-and-truthy → true, absent → false. Cannot fail.
#[must_use]
pub fn checkbox(form: &FormData, name: &str) -> bool {
    matches!(raw(form, name), Some("true" | "on" | "1"))
}

fn parse_datetime(v: &str) -> Option<NaiveDateTime> {
    // datetime-local inputs submit "2026-07-19T14:30" (seconds optional);
    // also accept the SQL-ish spaced form.
    for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(v, fmt) {
            return Some(dt);
        }
    }
    None
}

/// One bound of a timestamp range filter: a `datetime-local` value, or a
/// plain `date` meaning the start (`end = false`) or the last second
/// (`end = true`) of that day, so `to=2026-05-01` includes all of May 1st.
#[must_use]
pub fn filter_datetime(v: &str, end: bool) -> Option<NaiveDateTime> {
    parse_datetime(v).or_else(|| {
        let day = chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").ok()?;
        if end {
            day.and_hms_opt(23, 59, 59)
        } else {
            day.and_hms_opt(0, 0, 0)
        }
    })
}

/// A required timestamp column, from a `datetime-local` input.
pub fn req_datetime(
    form: &FormData,
    name: &'static str,
    errors: &mut FormErrors,
) -> Option<NaiveDateTime> {
    match raw(form, name) {
        None => {
            err(errors, name, "required");
            None
        }
        Some(v) => {
            if let Some(dt) = parse_datetime(v) {
                Some(dt)
            } else {
                err(errors, name, "invalid_datetime");
                None
            }
        }
    }
}

/// A nullable timestamp column: empty → `Some(None)`.
pub fn opt_datetime(
    form: &FormData,
    name: &'static str,
    errors: &mut FormErrors,
) -> Option<Option<NaiveDateTime>> {
    match raw(form, name) {
        None => Some(None),
        Some(v) => {
            if let Some(dt) = parse_datetime(v) {
                Some(Some(dt))
            } else {
                err(errors, name, "invalid_datetime");
                None
            }
        }
    }
}

/// Record one field error (what generated validation and the unique check
/// call; hooks can push into their `FormErrors` the same way).
pub fn push_error(errors: &mut FormErrors, field: &'static str, code: &'static str) {
    err(errors, field, code);
}

/// `#[ui(email)]`: one `@` with something on both sides and a dot in the
/// domain, no whitespace. Deliverability is the mail server's business —
/// this only rejects what can't be an address.
#[must_use]
pub fn valid_email(value: &str) -> bool {
    let v = value.trim();
    let Some((local, domain)) = v.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !v.chars().any(char::is_whitespace)
}

/// `#[ui(url)]`: an absolute `http://` or `https://` URL with a host. Any
/// other scheme — `javascript:`, `data:` — is rejected, because a stored URL
/// ends up in an `href` that autoescaping does not neutralize.
#[must_use]
pub fn valid_url(value: &str) -> bool {
    let v = value.trim();
    let lower = v.to_ascii_lowercase();
    let rest = if let Some(r) = lower.strip_prefix("https://") {
        r
    } else if let Some(r) = lower.strip_prefix("http://") {
        r
    } else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    !host.is_empty() && !v.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// `#[ui(min = .., max = ..)]` on a text column: bounds on the length in
/// characters (`too_short` / `too_long`).
pub fn check_length(
    errors: &mut FormErrors,
    field: &'static str,
    value: &str,
    min: Option<f64>,
    max: Option<f64>,
) {
    check_bounds(
        errors,
        field,
        usize_as_f64(value.chars().count()),
        min,
        max,
        ("too_short", "too_long"),
    );
}

/// `#[ui(min = .., max = ..)]` on a number column: inclusive bounds on the
/// value (`too_small` / `too_large`).
pub fn check_range(
    errors: &mut FormErrors,
    field: &'static str,
    value: f64,
    min: Option<f64>,
    max: Option<f64>,
) {
    check_bounds(errors, field, value, min, max, ("too_small", "too_large"));
}

fn check_bounds(
    errors: &mut FormErrors,
    field: &'static str,
    value: f64,
    min: Option<f64>,
    max: Option<f64>,
    codes: (&'static str, &'static str),
) {
    if min.is_some_and(|m| value < m) {
        err(errors, field, codes.0);
    } else if max.is_some_and(|m| value > m) {
        err(errors, field, codes.1);
    }
}

#[allow(clippy::cast_precision_loss)]
fn usize_as_f64(v: usize) -> f64 {
    v as f64
}

/// A number column's value as `f64` for bound checks (`#[ui(min/max)]`).
pub trait AsF64 {
    fn as_f64(&self) -> f64;
}

impl AsF64 for i64 {
    // Bounds are small literals; precision loss above 2^53 is irrelevant
    // for a comparison against them.
    #[allow(clippy::cast_precision_loss)]
    fn as_f64(&self) -> f64 {
        *self as f64
    }
}

impl AsF64 for f64 {
    fn as_f64(&self) -> f64 {
        *self
    }
}

/// On create, an omitted value of a column with a declared default gets
/// that default — the generated form shows such columns as optional.
pub fn fill_default(form: &mut FormData, column: &str, default: &str) {
    if raw(form, column).is_none() {
        form.insert(column.to_string(), default.to_string());
    }
}

/// `#[ui(slug_from = source)]`: when `column` was left empty, keep the
/// stored slug on edit (stable URLs) or derive it from `source` on create.
pub fn fill_slug(form: &mut FormData, column: &str, source: &str, stored: Option<&str>) {
    if raw(form, column).is_some() {
        return;
    }
    let slug = match stored.filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => crate::text::slugify(raw(form, source).unwrap_or("")),
    };
    form.insert(column.to_string(), slug);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(pairs: &[(&str, &str)]) -> FormData {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn required_and_optional_text() {
        let mut errors = Vec::new();
        let f = form(&[("a", "  x  "), ("b", "   ")]);
        assert_eq!(req_str(&f, "a", &mut errors), Some("x".into()));
        assert_eq!(opt_str(&f, "b"), None);
        assert_eq!(req_str(&f, "b", &mut errors), None);
        assert_eq!(req_str(&f, "missing", &mut errors), None);
        assert_eq!(
            errors,
            vec![
                FieldError {
                    field: "b",
                    code: "required"
                },
                FieldError {
                    field: "missing",
                    code: "required"
                },
            ]
        );
    }

    #[test]
    fn numbers_and_checkboxes() {
        let mut errors = Vec::new();
        let f = form(&[("n", "4.5"), ("bad", "abc"), ("cb", "on")]);
        assert_eq!(
            req_parse::<f64>(&f, "n", "invalid_number", &mut errors),
            Some(4.5)
        );
        assert_eq!(
            req_parse::<f64>(&f, "bad", "invalid_number", &mut errors),
            None
        );
        assert_eq!(
            opt_parse::<i64>(&f, "absent", "invalid_number", &mut errors),
            Some(None)
        );
        assert!(checkbox(&f, "cb"));
        assert!(!checkbox(&f, "absent"));
        assert_eq!(
            errors,
            vec![FieldError {
                field: "bad",
                code: "invalid_number"
            }]
        );
    }

    #[test]
    fn datetimes() {
        let mut errors = Vec::new();
        let f = form(&[("t", "2026-07-19T14:30")]);
        assert!(req_datetime(&f, "t", &mut errors).is_some());
        assert_eq!(opt_datetime(&f, "absent", &mut errors), Some(None));
        assert!(errors.is_empty());
    }

    #[test]
    fn email_and_url_validation() {
        assert!(valid_email("a@b.de"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("@b.de"));
        assert!(!valid_email("a b@c.de"));
        assert!(valid_url("https://example.com/x?y"));
        assert!(valid_url("http://localhost.test"));
        assert!(!valid_url("javascript:alert(1)"));
        assert!(!valid_url("JAVASCRIPT://x"));
        assert!(!valid_url("https://"));
        assert!(!valid_url("ftp://x.de"));
    }

    #[test]
    fn slug_fill_keeps_stored_and_derives_new() {
        let mut form: FormData = [("name".to_string(), "Hello World".to_string())].into();
        fill_slug(&mut form, "slug", "name", None);
        assert_eq!(form["slug"], "hello-world");
        let mut form: FormData = [("name".to_string(), "Other".to_string())].into();
        fill_slug(&mut form, "slug", "name", Some("kept"));
        assert_eq!(form["slug"], "kept");
        let mut form: FormData = [("slug".to_string(), "mine".to_string())].into();
        fill_slug(&mut form, "slug", "name", Some("kept"));
        assert_eq!(form["slug"], "mine");
    }

    #[test]
    fn bounds() {
        let mut errors = Vec::new();
        check_length(&mut errors, "n", "ab", Some(3.0), None);
        check_range(&mut errors, "p", 11.0, Some(0.0), Some(10.0));
        check_range(&mut errors, "q", 5.0, Some(0.0), Some(10.0));
        let codes: Vec<&str> = errors.iter().map(|e| e.code).collect();
        assert_eq!(codes, ["too_short", "too_large"]);
    }
}
