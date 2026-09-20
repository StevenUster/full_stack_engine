//! `serde` helpers for HTML form structs.
//!
//! An HTML form never omits a field: an untouched `<input type="number">`
//! submits `""`, not nothing. `Option<i64>` therefore fails to deserialize
//! rather than arriving as `None`, which is why every app that has a form with
//! an optional number ends up writing the same custom deserializer. This is
//! that deserializer, once.
//!
//! ```ignore
//! #[derive(Deserialize)]
//! struct RunForm {
//!     name: String,
//!     #[serde(default, deserialize_with = "full_stack_engine::forms::empty_as_none")]
//!     distance_km: Option<f64>,
//!     #[serde(deserialize_with = "full_stack_engine::forms::trimmed")]
//!     location: String,
//! }
//! ```

use serde::{Deserialize, Deserializer};

/// Deserializes a form field into `Option<T>`, treating a missing, blank or
/// whitespace-only value as `None` and parsing anything else with
/// [`std::str::FromStr`].
///
/// Pair with `#[serde(default)]` so a field the browser omits entirely (an
/// unchecked checkbox, a disabled input) is also `None` rather than an error.
///
/// # Errors
///
/// Returns a deserialization error if the value is non-blank and does not
/// parse as `T` — a typo'd number is a bad request, not a silent `None`.
pub fn empty_as_none<'de, D, T>(de: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let raw = Option::<String>::deserialize(de)?;
    match raw.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => value.parse().map(Some).map_err(serde::de::Error::custom),
    }
}

/// Deserializes a string field with the surrounding whitespace removed.
///
/// Users paste values with a trailing space constantly, and an untrimmed
/// `String` then defeats uniqueness checks and exact-match lookups — an email
/// of `"a@b.com "` is a different row from `"a@b.com"`.
///
/// # Errors
///
/// Returns a deserialization error only if the field is not a string.
pub fn trimmed<'de, D>(de: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(String::deserialize(de)?.trim().to_string())
}

/// Like [`trimmed`], but a field that trims to nothing becomes `None`.
///
/// # Errors
///
/// Returns a deserialization error only if the field is not a string.
pub fn trimmed_or_none<'de, D>(de: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(de)?;
    Ok(raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
}

/// Parses a decimal number written with either separator — `"1234.50"` or the
/// German/French `"1234,50"` — returning `None` for blank or unparseable
/// input.
///
/// Form fields are typed by humans, and a `<input type="number">` on a German
/// locale browser submits a comma. `str::parse::<f64>` rejects that, so the
/// amount silently became zero (or an error) depending on who filled the form
/// in.
///
/// Note this only accepts a *decimal* comma, not thousands separators:
/// `"1,234.50"` is ambiguous across locales and is rejected rather than
/// guessed at.
///
/// ```
/// # use full_stack_engine::forms::parse_decimal;
/// assert_eq!(parse_decimal("12,50"), Some(12.5));
/// assert_eq!(parse_decimal(" 12.50 "), Some(12.5));
/// assert_eq!(parse_decimal(""), None);
/// assert_eq!(parse_decimal("1,234.50"), None);
/// ```
#[must_use]
pub fn parse_decimal(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.matches([',', '.']).count() > 1 {
        return None;
    }
    trimmed.replace(',', ".").parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Form {
        #[serde(default, deserialize_with = "empty_as_none")]
        count: Option<i64>,
        #[serde(default, deserialize_with = "trimmed")]
        name: String,
        #[serde(default, deserialize_with = "trimmed_or_none")]
        note: Option<String>,
    }

    fn parse(json: &str) -> Result<Form, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn blank_and_missing_numbers_become_none() {
        assert_eq!(parse(r#"{"count":"","name":"x"}"#).unwrap().count, None);
        assert_eq!(parse(r#"{"count":"  ","name":"x"}"#).unwrap().count, None);
        assert_eq!(parse(r#"{"name":"x"}"#).unwrap().count, None);
        assert_eq!(parse(r#"{"count":"7","name":"x"}"#).unwrap().count, Some(7));
    }

    #[test]
    fn an_unparseable_number_is_an_error_not_a_none() {
        // The alternative — swallowing it as `None` — turns "user typed 1o"
        // into "user left it blank", which then passes a required-field check.
        assert!(parse(r#"{"count":"1o","name":"x"}"#).is_err());
    }

    #[test]
    fn strings_are_trimmed_and_blank_options_dropped() {
        let form = parse(r#"{"name":"  Ada  ","note":"  "}"#).unwrap();
        assert_eq!(form.name, "Ada");
        assert_eq!(form.note, None);
        assert_eq!(
            parse(r#"{"name":"a","note":" hi "}"#)
                .unwrap()
                .note
                .as_deref(),
            Some("hi")
        );
    }

    #[test]
    fn decimals_parse_with_either_separator() {
        assert_eq!(parse_decimal("12,50"), Some(12.5));
        assert_eq!(parse_decimal("12.50"), Some(12.5));
        assert_eq!(parse_decimal(" 0,01 "), Some(0.01));
        assert_eq!(parse_decimal("7"), Some(7.0));
        assert_eq!(parse_decimal(""), None);
        assert_eq!(parse_decimal("abc"), None);
        // Ambiguous across locales — rejected rather than guessed at.
        assert_eq!(parse_decimal("1,234.50"), None);
        assert_eq!(parse_decimal("1.234,50"), None);
    }
}
