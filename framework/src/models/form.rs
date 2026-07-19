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
                FieldError { field: "b", code: "required" },
                FieldError { field: "missing", code: "required" },
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
        assert_eq!(opt_parse::<i64>(&f, "absent", "invalid_number", &mut errors), Some(None));
        assert!(checkbox(&f, "cb"));
        assert!(!checkbox(&f, "absent"));
        assert_eq!(errors, vec![FieldError { field: "bad", code: "invalid_number" }]);
    }

    #[test]
    fn datetimes() {
        let mut errors = Vec::new();
        let f = form(&[("t", "2026-07-19T14:30")]);
        assert!(req_datetime(&f, "t", &mut errors).is_some());
        assert_eq!(opt_datetime(&f, "absent", &mut errors), Some(None));
        assert!(errors.is_empty());
    }
}
