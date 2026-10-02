//! Locale-aware Tera filters, registered on every app's template engine at
//! boot.
//!
//! Before these existed, a template could only render what a handler had
//! already turned into a string, so every app grew a set of
//! `format_date_de` / `format_eur` helpers in Rust — string surgery on ISO
//! dates, duplicated per app and per format, and invisible to the theme
//! author who actually decides how a date should look.
//!
//! A date is presentation. It belongs in the template:
//!
//! ```jinja
//! {{ event.date | date }}                  {# 29.06.2026 in de, 06/29/2026 in en-US #}
//! {{ event.starts_at | datetime }}         {# 29.06.2026 14:30 #}
//! {{ order.total | currency }}             {# 1.234,50 € #}
//! {{ product.name | slugify }}             {# summer-sale-2026 #}
//! ```
//!
//! # Which locale
//!
//! The filters format for the app's default language, which covers the common
//! single-language app. A multi-language theme passes the request's language
//! explicitly — `lang` is in every render context:
//!
//! ```jinja
//! {{ event.date | date(locale=lang) }}
//! ```
//!
//! An unknown language falls back to ISO 8601 (`2026-06-29`) and a `.`
//! decimal separator, which are unambiguous everywhere rather than wrong
//! somewhere.
//!
//! # Which formats
//!
//! [`LocaleFormat::for_lang`] holds the table. It is deliberately small and
//! explicit rather than a full CLDR dependency: these cover date order,
//! separator and currency placement, which is what actually differs between
//! the languages an app of this kind serves. An app needing more passes an
//! explicit `format` (a [`chrono`] format string) to the filter.

use std::collections::HashMap;

use serde_json::Value;
use tera::{Error, Tera, to_value};

/// How one language writes dates and numbers.
#[derive(Copy, Clone)]
pub struct LocaleFormat {
    /// `chrono` format string for a date.
    pub date: &'static str,
    /// `chrono` format string for a date and time.
    pub datetime: &'static str,
    /// `chrono` format string for a time of day.
    pub time: &'static str,
    /// Decimal separator.
    pub decimal: char,
    /// Thousands separator, or `None` for no grouping.
    pub group: Option<char>,
    /// Whether the currency symbol follows the amount (`1,50 €`) rather than
    /// preceding it (`$1.50`).
    pub currency_after: bool,
}

/// ISO 8601 dates and a `.` decimal point: the fallback for any language not
/// in the table, chosen because it is unambiguous in every locale rather than
/// natural in one.
pub const ISO: LocaleFormat = LocaleFormat {
    date: "%Y-%m-%d",
    datetime: "%Y-%m-%d %H:%M",
    time: "%H:%M",
    decimal: '.',
    group: None,
    currency_after: false,
};

const DOT_DMY: LocaleFormat = LocaleFormat {
    date: "%d.%m.%Y",
    datetime: "%d.%m.%Y %H:%M",
    time: "%H:%M",
    decimal: ',',
    group: Some('.'),
    currency_after: true,
};

const SLASH_DMY: LocaleFormat = LocaleFormat {
    date: "%d/%m/%Y",
    datetime: "%d/%m/%Y %H:%M",
    time: "%H:%M",
    decimal: ',',
    group: Some('.'),
    currency_after: true,
};

impl LocaleFormat {
    /// The formats for a language tag (`"de"`, `"de-CH"`, `"en-US"`), falling
    /// back to [`ISO`] for anything unlisted.
    ///
    /// Matched on the full tag first, then on the primary subtag, so `en-US`
    /// gets American ordering while a bare `en` gets British.
    #[must_use]
    pub fn for_lang(lang: &str) -> Self {
        let tag = lang.trim().to_lowercase();
        match tag.as_str() {
            "en-us" => {
                return LocaleFormat {
                    date: "%m/%d/%Y",
                    datetime: "%m/%d/%Y %I:%M %p",
                    time: "%I:%M %p",
                    decimal: '.',
                    group: Some(','),
                    currency_after: false,
                };
            }
            "de-ch" | "it-ch" => {
                return LocaleFormat {
                    decimal: '.',
                    group: Some('\''),
                    ..DOT_DMY
                };
            }
            _ => {}
        }
        match tag.split(['-', '_']).next().unwrap_or("") {
            // Dot-separated day-first, comma decimal: the German-speaking and
            // Nordic/Slavic group.
            "de" | "at" | "cs" | "da" | "et" | "fi" | "is" | "nb" | "nn" | "no" | "pl" | "ro"
            | "ru" | "sk" | "sl" | "sr" | "tr" | "uk" => DOT_DMY,
            // Slash-separated day-first, comma decimal.
            "es" | "fr" | "id" | "it" | "nl" | "pt" | "vi" => SLASH_DMY,
            // British English: day-first slashes, but a `.` decimal point.
            "en" => LocaleFormat {
                date: "%d/%m/%Y",
                datetime: "%d/%m/%Y %H:%M",
                time: "%H:%M",
                decimal: '.',
                group: Some(','),
                currency_after: false,
            },
            // Year-first, and a currency symbol in front.
            "ja" | "ko" | "zh" | "sv" | "hu" | "lt" => LocaleFormat {
                decimal: ',',
                group: Some(' '),
                ..ISO
            },
            _ => ISO,
        }
    }
}

/// The currency symbol for an ISO 4217 code, or the code itself when there is
/// no well-known symbol — `"SEK"` is what a Swedish price tag says, so
/// printing the code is correct rather than a fallback.
#[must_use]
pub fn currency_symbol(code: &str) -> String {
    match code.trim().to_uppercase().as_str() {
        "EUR" => "€".to_string(),
        "USD" => "$".to_string(),
        "GBP" => "£".to_string(),
        "JPY" | "CNY" => "¥".to_string(),
        "CHF" => "CHF".to_string(),
        "PLN" => "zł".to_string(),
        "SEK" | "NOK" | "DKK" | "ISK" => "kr".to_string(),
        other => other.to_string(),
    }
}

/// Registers every filter on `tera`, formatting for `default_lang` unless a
/// call passes `locale=`, and using `currency` as the default currency code.
///
/// Called by [`crate::FrameworkApp::run`]; an app only needs this directly
/// when it builds its own [`Tera`] (see [`crate::testing::load_themes`]).
pub fn register(tera: &mut Tera, default_lang: &str, currency: Option<String>) {
    let lang = default_lang.to_string();

    let date_lang = lang.clone();
    tera.register_filter(
        "date",
        move |value: &Value, args: &HashMap<String, Value>| {
            format_temporal(value, args, &date_lang, Part::Date)
        },
    );

    let datetime_lang = lang.clone();
    tera.register_filter(
        "datetime",
        move |value: &Value, args: &HashMap<String, Value>| {
            format_temporal(value, args, &datetime_lang, Part::DateTime)
        },
    );

    let time_lang = lang.clone();
    tera.register_filter(
        "time",
        move |value: &Value, args: &HashMap<String, Value>| {
            format_temporal(value, args, &time_lang, Part::Time)
        },
    );

    let number_lang = lang.clone();
    tera.register_filter(
        "number",
        move |value: &Value, args: &HashMap<String, Value>| {
            format_number_value(value, args, &number_lang)
        },
    );

    let currency_lang = lang.clone();
    let default_currency = currency;
    tera.register_filter(
        "currency",
        move |value: &Value, args: &HashMap<String, Value>| {
            format_currency_value(value, args, &currency_lang, default_currency.as_deref())
        },
    );

    tera.register_filter("slugify", |value: &Value, _: &HashMap<String, Value>| {
        let Some(text) = value.as_str() else {
            return Ok(value.clone());
        };
        Ok(to_value(crate::text::slugify(text))?)
    });
}

/// Formats `value` the way the `kind` filter would (`date`, `datetime`,
/// `time`, `number`, `currency`) for `lang` — the server-side twin of the
/// Tera filters, used for `#[ui(format = ...)]` columns so a page gets a
/// ready `{col}_display` string. Unknown kinds and values that don't parse
/// come back unchanged.
#[must_use]
pub fn format(kind: &str, value: &Value, lang: &str, currency: Option<&str>) -> Value {
    let args = HashMap::new();
    let out = match kind {
        "date" => format_temporal(value, &args, lang, Part::Date),
        "datetime" => format_temporal(value, &args, lang, Part::DateTime),
        "time" => format_temporal(value, &args, lang, Part::Time),
        "number" => format_number_value(value, &args, lang),
        "currency" => format_currency_value(value, &args, lang, currency),
        _ => Ok(value.clone()),
    };
    out.unwrap_or_else(|_| value.clone())
}

fn format_number_value(
    value: &Value,
    args: &HashMap<String, Value>,
    default_lang: &str,
) -> Result<Value, Error> {
    let fmt = LocaleFormat::for_lang(locale_arg(args, default_lang).as_str());
    let Some(number) = as_f64(value) else {
        return Ok(value.clone());
    };
    let precision = args.get("precision").and_then(Value::as_u64).map_or_else(
        || natural_precision(number),
        |p| usize::try_from(p).unwrap_or(2),
    );
    Ok(to_value(format_number(number, precision, &fmt))?)
}

fn format_currency_value(
    value: &Value,
    args: &HashMap<String, Value>,
    default_lang: &str,
    default_currency: Option<&str>,
) -> Result<Value, Error> {
    let fmt = LocaleFormat::for_lang(locale_arg(args, default_lang).as_str());
    let Some(number) = as_f64(value) else {
        return Ok(value.clone());
    };
    let precision = args
        .get("precision")
        .and_then(Value::as_u64)
        .map_or(2, |p| usize::try_from(p).unwrap_or(2));
    let amount = format_number(number, precision, &fmt);

    let code = args
        .get("code")
        .and_then(Value::as_str)
        .or(default_currency);
    let Some(code) = code else {
        // No currency configured and none passed: the formatted number
        // alone, rather than inventing a symbol.
        return Ok(to_value(amount)?);
    };
    let symbol = currency_symbol(code);
    Ok(to_value(if fmt.currency_after {
        format!("{amount}\u{a0}{symbol}")
    } else {
        format!("{symbol}{amount}")
    })?)
}

#[derive(Copy, Clone)]
enum Part {
    Date,
    DateTime,
    Time,
}

fn locale_arg(args: &HashMap<String, Value>, default: &str) -> String {
    args.get("locale")
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_string()
}

/// Formats a stored timestamp. Anything that does not parse as a date is
/// returned unchanged rather than raising: a template should not 500 because
/// one row holds a legacy value, and an unformatted date is visible in review.
fn format_temporal(
    value: &Value,
    args: &HashMap<String, Value>,
    default_lang: &str,
    part: Part,
) -> Result<Value, Error> {
    let Some(raw) = value.as_str() else {
        return Ok(value.clone());
    };
    let fmt = LocaleFormat::for_lang(locale_arg(args, default_lang).as_str());
    let pattern = args.get("format").and_then(Value::as_str);

    let Some(parsed) = parse_temporal(raw) else {
        return Ok(value.clone());
    };

    let out = match (parsed, part) {
        // A date-only value asked to render a time has no time to render; the
        // date is the honest answer, not `00:00`.
        (Parsed::Date(d), Part::Date | Part::Time) => {
            d.format(pattern.unwrap_or(fmt.date)).to_string()
        }
        (Parsed::Date(d), Part::DateTime) => d.format(pattern.unwrap_or(fmt.date)).to_string(),
        (Parsed::DateTime(dt), Part::Date) => dt.format(pattern.unwrap_or(fmt.date)).to_string(),
        (Parsed::DateTime(dt), Part::DateTime) => {
            dt.format(pattern.unwrap_or(fmt.datetime)).to_string()
        }
        (Parsed::DateTime(dt), Part::Time) => dt.format(pattern.unwrap_or(fmt.time)).to_string(),
    };
    Ok(to_value(out)?)
}

enum Parsed {
    Date(chrono::NaiveDate),
    DateTime(chrono::NaiveDateTime),
}

/// Parses the shapes a `SQLite` column actually holds: what the ORM writes for
/// a `TIMESTAMP` (`2026-06-29T14:30:00`), what `datetime-local` inputs submit
/// (`2026-06-29T14:30`), the space-separated variant, an RFC 3339 value with an
/// offset, and a plain date.
fn parse_temporal(raw: &str) -> Option<Parsed> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        return Some(Parsed::Date(date));
    }
    for pattern in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(trimmed, pattern) {
            return Some(Parsed::DateTime(dt));
        }
    }
    // With an offset: render the instant as the author wrote it rather than
    // converting to a server timezone nobody chose.
    chrono::DateTime::parse_from_rfc3339(trimmed)
        .ok()
        .map(|dt| Parsed::DateTime(dt.naive_local()))
}

fn as_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(crate::forms::parse_decimal))
}

/// Two decimals for a fractional value, none for a whole one — so a quantity
/// of `3` does not render as `3.00` while a price of `2.5` still shows its
/// cents.
fn natural_precision(number: f64) -> usize {
    if (number.fract()).abs() < f64::EPSILON {
        0
    } else {
        2
    }
}

/// Rounds a tie away from zero rather than to the nearest even digit.
///
/// `format!("{:.0}", 1234.5)` yields `1234`: Rust rounds half to even, which is
/// right for statistics and wrong for a price. Every spreadsheet, invoice and
/// cash register rounds `.5` up, and a total that disagrees with the
/// customer's own arithmetic is a support ticket.
///
/// Only an *exact* binary tie is adjusted. A value like `2.675`, which is
/// really `2.67499...` in binary, is not a tie at all, and the formatter's
/// correctly-rounded `2.67` is the honest answer — reaching `2.68` would need
/// decimal arithmetic, and the precision was already lost before this filter
/// ever saw the number.
fn round_half_away_from_zero(number: f64, precision: usize) -> f64 {
    let factor = 10f64.powi(i32::try_from(precision).unwrap_or(2));
    let scaled = number * factor;
    if !scaled.is_finite() {
        return number;
    }
    if (scaled.fract().abs() - 0.5).abs() < f64::EPSILON {
        return scaled.abs().ceil().copysign(number) / factor;
    }
    number
}

fn format_number(number: f64, precision: usize, fmt: &LocaleFormat) -> String {
    let number = round_half_away_from_zero(number, precision);
    let rendered = format!("{number:.precision$}");
    let (sign, digits) = match rendered.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", rendered.as_str()),
    };
    let (int_part, frac_part) = digits.split_once('.').unwrap_or((digits, ""));

    let mut grouped = String::with_capacity(int_part.len() + int_part.len() / 3);
    if let Some(sep) = fmt.group {
        for (i, ch) in int_part.chars().enumerate() {
            if i > 0 && (int_part.len() - i) % 3 == 0 {
                grouped.push(sep);
            }
            grouped.push(ch);
        }
    } else {
        grouped.push_str(int_part);
    }

    if frac_part.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}{}{frac_part}", fmt.decimal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(template: &str, context: &serde_json::Value, lang: &str) -> String {
        let mut tera = Tera::default();
        tera.add_raw_template("t", template).unwrap();
        register(&mut tera, lang, Some("EUR".to_string()));
        tera.render("t", &tera::Context::from_serialize(context).unwrap())
            .unwrap()
    }

    #[test]
    fn dates_follow_the_language() {
        let ctx = serde_json::json!({ "d": "2026-06-29" });
        assert_eq!(render("{{ d | date }}", &ctx, "de"), "29.06.2026");
        assert_eq!(render("{{ d | date }}", &ctx, "fr"), "29/06/2026");
        assert_eq!(render("{{ d | date }}", &ctx, "en-US"), "06/29/2026");
        assert_eq!(render("{{ d | date }}", &ctx, "ja"), "2026-06-29");
        // An unlisted language gets ISO rather than a guess.
        assert_eq!(render("{{ d | date }}", &ctx, "xx"), "2026-06-29");
    }

    #[test]
    fn the_locale_argument_overrides_the_default() {
        let ctx = serde_json::json!({ "d": "2026-06-29", "lang": "de" });
        assert_eq!(
            render("{{ d | date(locale=lang) }}", &ctx, "en-US"),
            "29.06.2026"
        );
    }

    #[test]
    fn timestamps_render_date_time_or_both() {
        let ctx = serde_json::json!({ "t": "2026-06-29T14:30:00" });
        assert_eq!(render("{{ t | date }}", &ctx, "de"), "29.06.2026");
        assert_eq!(render("{{ t | datetime }}", &ctx, "de"), "29.06.2026 14:30");
        assert_eq!(render("{{ t | time }}", &ctx, "de"), "14:30");
        // The shape an HTML `datetime-local` input submits.
        let short = serde_json::json!({ "t": "2026-06-29T14:30" });
        assert_eq!(
            render("{{ t | datetime }}", &short, "de"),
            "29.06.2026 14:30"
        );
        // And the space-separated shape SQLite holds.
        let spaced = serde_json::json!({ "t": "2026-06-29 14:30:00" });
        assert_eq!(
            render("{{ t | datetime }}", &spaced, "de"),
            "29.06.2026 14:30"
        );
    }

    #[test]
    fn a_date_only_value_asked_for_a_datetime_does_not_invent_midnight() {
        let ctx = serde_json::json!({ "d": "2026-06-29" });
        assert_eq!(render("{{ d | datetime }}", &ctx, "de"), "29.06.2026");
        assert_eq!(render("{{ d | time }}", &ctx, "de"), "29.06.2026");
    }

    #[test]
    fn unparseable_values_pass_through_instead_of_failing_the_page() {
        let ctx = serde_json::json!({ "d": "not a date", "e": "", "n": null });
        assert_eq!(render("{{ d | date }}", &ctx, "de"), "not a date");
        assert_eq!(render("{{ e | date }}", &ctx, "de"), "");
        assert_eq!(render("{{ n | date }}", &ctx, "de"), "");
    }

    #[test]
    fn currency_places_the_symbol_where_the_locale_puts_it() {
        // The currency itself is the app's, not the reader's: a German shop
        // priced in euro still charges euro to an American visitor. Only the
        // separators and the symbol's *position* follow the language.
        let ctx = serde_json::json!({ "a": 1234.5 });
        assert_eq!(render("{{ a | currency }}", &ctx, "de"), "1.234,50\u{a0}€");
        assert_eq!(render("{{ a | currency }}", &ctx, "en-US"), "€1,234.50");
        assert_eq!(
            render("{{ a | currency(code=\"GBP\") }}", &ctx, "en"),
            "£1,234.50"
        );
    }

    #[test]
    fn an_app_with_no_currency_configured_gets_the_bare_number() {
        let mut tera = Tera::default();
        tera.add_raw_template("t", "{{ a | currency }}").unwrap();
        register(&mut tera, "de", None);
        let ctx = tera::Context::from_serialize(serde_json::json!({ "a": 1234.5 })).unwrap();
        // Inventing a symbol would be worse than omitting one.
        assert_eq!(tera.render("t", &ctx).unwrap(), "1.234,50");
    }

    #[test]
    fn a_half_is_rounded_up_the_way_an_invoice_does() {
        let ctx = serde_json::json!({ "a": 1234.5, "b": 1235.5, "c": 0.125, "d": -1234.5 });
        // Rust's own `{:.0}` would give 1234 and 1236 here — even-rounding,
        // which makes two adjacent rows disagree about what .5 means.
        assert_eq!(render("{{ a | number(precision=0) }}", &ctx, "de"), "1.235");
        assert_eq!(render("{{ b | number(precision=0) }}", &ctx, "de"), "1.236");
        assert_eq!(render("{{ c | currency }}", &ctx, "de"), "0,13\u{a0}€");
        // Away from zero, symmetrically.
        assert_eq!(
            render("{{ d | number(precision=0) }}", &ctx, "de"),
            "-1.235"
        );
    }

    #[test]
    fn currency_accepts_the_string_forms_a_form_submits() {
        // A comma decimal typed into a form, and a plain string from a TEXT
        // column, both format rather than passing through unformatted.
        let ctx = serde_json::json!({ "a": "1234,5", "b": "1234.5" });
        assert_eq!(render("{{ a | currency }}", &ctx, "de"), "1.234,50\u{a0}€");
        assert_eq!(render("{{ b | currency }}", &ctx, "de"), "1.234,50\u{a0}€");
    }

    #[test]
    fn numbers_group_and_keep_natural_precision() {
        let ctx = serde_json::json!({ "whole": 1_234_567, "frac": 1234.5, "neg": -1234.5 });
        assert_eq!(render("{{ whole | number }}", &ctx, "de"), "1.234.567");
        assert_eq!(render("{{ frac | number }}", &ctx, "de"), "1.234,50");
        assert_eq!(render("{{ neg | number }}", &ctx, "de"), "-1.234,50");
        assert_eq!(
            render("{{ frac | number(precision=0) }}", &ctx, "de"),
            "1.235"
        );
        assert_eq!(render("{{ whole | number }}", &ctx, "xx"), "1234567");
    }

    #[test]
    fn slugify_is_available_to_templates() {
        let ctx = serde_json::json!({ "n": "Summer Sale 2026!" });
        assert_eq!(render("{{ n | slugify }}", &ctx, "de"), "summer-sale-2026");
    }
}
