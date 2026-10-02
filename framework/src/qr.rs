//! QR codes as inline `data:` URIs.
//!
//! Rendered as SVG and base64-encoded so the result drops straight into an
//! `<img src>` — which is what makes it work in the two places a QR code is
//! usually needed and an extra HTTP request is not available: a generated PDF
//! (see the `pdf` module (feature `pdf`), whose renderer blocks every non-`data:` subresource on
//! purpose) and an HTML email.
//!
//! ```ignore
//! let src = qr::svg_data_uri("https://example.com/ticket/42", 180)?;
//! // then in the template: <img src="{{ qr }}" alt="">
//! ```

use base64::Engine;

/// Renders `payload` as an SVG QR code and returns it as a
/// `data:image/svg+xml;base64,…` URI, at least `size` CSS pixels square.
///
/// Returns `None` for an empty payload, or one too large to encode (roughly
/// 2 KB at this error-correction level) — the caller decides whether a missing
/// code is worth failing over, which is not something this function can know.
#[must_use]
pub fn svg_data_uri(payload: &str, size: u32) -> Option<String> {
    if payload.is_empty() {
        return None;
    }
    // Medium error correction: ~15% recoverable, the usual choice for something
    // printed onto a letter that may be folded or scanned at an angle.
    let code =
        qrcode::QrCode::with_error_correction_level(payload.as_bytes(), qrcode::EcLevel::M).ok()?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(size, size)
        // The quiet zone is part of the specification, not decoration: without
        // it many scanners will not see the code at all.
        .quiet_zone(true)
        .build();
    Some(format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(svg.as_bytes())
    ))
}

/// A SEPA credit transfer, as encoded by a `GiroCode`.
///
/// Scanning one fills in a banking app's transfer form, which is why it appears
/// on invoices and donation receipts across the euro area. Only `iban` and
/// `holder` are mandatory; leaving `amount` unset lets the payer choose it.
pub struct SepaTransfer<'a> {
    /// The recipient's IBAN. Spaces are removed.
    pub iban: &'a str,
    /// BIC. Optional since SEPA made it so; pass `""` to omit.
    pub bic: &'a str,
    /// Account holder, truncated to the specification's 70 characters.
    pub holder: &'a str,
    /// Remittance information ("what is this payment for"), truncated to 140.
    pub reference: &'a str,
    /// Amount in euro. `None` leaves the field open for the payer.
    pub amount: Option<f64>,
}

impl SepaTransfer<'_> {
    /// Builds the EPC069-12 payload: eleven newline-separated fields,
    /// version `002` (which is the version that makes the BIC optional).
    ///
    /// Returns `None` when a mandatory field is missing, because a QR code
    /// that opens a banking app with no recipient is worse than no QR code.
    #[must_use]
    pub fn payload(&self) -> Option<String> {
        let iban = self.iban.replace(char::is_whitespace, "");
        let holder: String = self.holder.trim().chars().take(70).collect();
        if iban.is_empty() || holder.is_empty() {
            return None;
        }
        let reference: String = self.reference.trim().chars().take(140).collect();
        // The specification's amount format: `EUR` followed by up to two
        // decimals, with a `.` separator regardless of the payer's locale.
        let amount = self
            .amount
            .filter(|a| *a > 0.0)
            .map_or_else(String::new, |a| format!("EUR{a:.2}"));

        Some(format!(
            "BCD\n002\n1\nSCT\n{bic}\n{holder}\n{iban}\n{amount}\n\n\n{reference}",
            bic = self.bic.trim(),
        ))
    }

    /// The transfer as a scannable SVG `data:` URI, or `None` if a mandatory
    /// field is missing.
    #[must_use]
    pub fn data_uri(&self, size: u32) -> Option<String> {
        svg_data_uri(&self.payload()?, size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_data_uri_is_produced_and_is_an_svg() {
        let uri = svg_data_uri("https://example.com/ticket/42", 180).unwrap();
        assert!(uri.starts_with("data:image/svg+xml;base64,"));
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(uri.trim_start_matches("data:image/svg+xml;base64,"))
            .unwrap();
        let svg = String::from_utf8(decoded).unwrap();
        assert!(svg.contains("<svg"), "{svg:.80}");
    }

    #[test]
    fn empty_and_oversized_payloads_return_none() {
        assert!(svg_data_uri("", 180).is_none());
        assert!(svg_data_uri(&"x".repeat(10_000), 180).is_none());
    }

    #[test]
    fn the_sepa_payload_has_the_specified_shape() {
        let payload = SepaTransfer {
            iban: "DE12 3456 7890 1234 5678 90",
            bic: "ABCDDEFFXXX",
            holder: "Example e.V.",
            reference: "Invoice 42",
            amount: Some(43.0),
        }
        .payload()
        .unwrap();

        let fields: Vec<&str> = payload.split('\n').collect();
        assert_eq!(fields.len(), 11, "EPC069-12 has 11 fields: {fields:?}");
        assert_eq!(fields[0], "BCD");
        assert_eq!(fields[1], "002");
        assert_eq!(fields[3], "SCT");
        assert_eq!(fields[4], "ABCDDEFFXXX");
        assert_eq!(fields[5], "Example e.V.");
        // Whitespace is what humans write IBANs with and what banks reject.
        assert_eq!(fields[6], "DE123456789012345678 90".replace(' ', ""));
        assert_eq!(fields[7], "EUR43.00");
        assert_eq!(fields[10], "Invoice 42");
    }

    #[test]
    fn an_open_amount_leaves_the_field_empty() {
        let payload = SepaTransfer {
            iban: "DE12345678901234567890",
            bic: "",
            holder: "Example",
            reference: "",
            amount: None,
        }
        .payload()
        .unwrap();
        let fields: Vec<&str> = payload.split('\n').collect();
        assert_eq!(fields[4], "", "BIC is optional in version 002");
        assert_eq!(fields[7], "", "an unset amount is an empty field");
        // A zero or negative amount is treated as unset rather than encoded.
        let zero = SepaTransfer {
            amount: Some(0.0),
            ..SepaTransfer {
                iban: "DE12345678901234567890",
                bic: "",
                holder: "Example",
                reference: "",
                amount: None,
            }
        }
        .payload()
        .unwrap();
        assert_eq!(zero.split('\n').nth(7).unwrap(), "");
    }

    #[test]
    fn a_transfer_without_recipient_details_is_refused() {
        let no_iban = SepaTransfer {
            iban: "   ",
            bic: "",
            holder: "Example",
            reference: "",
            amount: None,
        };
        assert!(no_iban.payload().is_none());
        assert!(no_iban.data_uri(180).is_none());

        let no_holder = SepaTransfer {
            iban: "DE12345678901234567890",
            bic: "",
            holder: "  ",
            reference: "",
            amount: None,
        };
        assert!(no_holder.payload().is_none());
    }

    #[test]
    fn long_fields_are_truncated_to_the_specified_lengths() {
        let payload = SepaTransfer {
            iban: "DE12345678901234567890",
            bic: "",
            holder: &"h".repeat(200),
            reference: &"r".repeat(300),
            amount: None,
        }
        .payload()
        .unwrap();
        let fields: Vec<&str> = payload.split('\n').collect();
        assert_eq!(fields[5].chars().count(), 70);
        assert_eq!(fields[10].chars().count(), 140);
    }
}
