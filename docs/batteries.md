# Batteries

The framework's stated aim is that an app be written in as little code as
possible, and that starting one not require choosing crates. This page lists
what the framework now does so an app doesn't have to. Everything here was
extracted from a real app after being written there first.

All of it is `use full_stack_engine::prelude::*` away.

---

## Formatting: Tera filters, not Rust helpers

A date is presentation. Before these filters, a template could only render what
a handler had already turned into a string, so apps grew `format_date_de` /
`format_eur` helpers — ISO string surgery, duplicated per app and per format,
and invisible to the person who actually decides how a date should look.

```jinja
{{ event.date | date }}              {# 29.06.2026 in de, 06/29/2026 in en-US #}
{{ event.starts_at | datetime }}     {# 29.06.2026 14:30 #}
{{ event.starts_at | time }}         {# 14:30 #}
{{ order.total | currency }}         {# 1.234,50 € #}
{{ count | number(precision=0) }}    {# 1.235 #}
{{ product.name | slugify }}         {# summer-sale-2026 #}
```

**Which locale.** The filters format for the app's default language. A
multi-language theme passes the request's language, which is in every render
context: `{{ event.date | date(locale=lang) }}`. An unlisted language falls
back to ISO 8601 and a `.` decimal separator — unambiguous everywhere rather
than wrong somewhere. The table is `filters::LocaleFormat::for_lang`; an app
needing something it doesn't cover passes `format=` (a `chrono` format string).

**Which currency.** `CURRENCY` (ISO 4217). The currency is the app's, not the
reader's — a German shop priced in euro still charges euro to an American
visitor — so only the separators and the symbol's position follow the
language. Unset renders the amount with no symbol rather than guessing one.

**Rounding.** `.5` rounds away from zero, not to the nearest even digit.
`format!("{:.0}", 1234.5)` gives `1234` in Rust, which is right for statistics
and wrong for a price: every invoice and cash register rounds it up, and a
total that disagrees with the customer's arithmetic is a support ticket.

**A value that doesn't parse passes through unchanged** rather than raising. A
page should not 500 because one row holds a legacy value, and an unformatted
date is visible in review.

> **From `.astro` pages.** These are Tera filters, so they work in any template
> the framework renders — pages written as Tera, emails, PDF documents. Reaching
> them from an `.astro` page needs a helper in `fse-ssr`, which compiles Astro
> expressions to Tera; that isn't built yet.

## Forms: the shapes a browser actually submits

An HTML form never omits a field — an untouched `<input type="number">`
submits `""`, not nothing — so `Option<i64>` fails to deserialize instead of
arriving as `None`.

```rust
#[derive(Deserialize)]
struct RunForm {
    #[serde(default, deserialize_with = "forms::empty_as_none")]
    distance_km: Option<f64>,
    #[serde(deserialize_with = "forms::trimmed")]
    email: String,
    #[serde(default, deserialize_with = "forms::trimmed_or_none")]
    note: Option<String>,
}
```

`empty_as_none` treats blank as `None` but a *malformed* value as an error:
swallowing `"1o"` as `None` would turn "the user typed a typo" into "the user
left it blank", which then passes a required-field check.

`forms::parse_decimal` accepts either separator (`"12,50"` and `"12.50"`),
because a `<input type="number">` on a German-locale browser submits a comma.
It rejects `"1,234.50"` rather than guessing: that's ambiguous across locales.

## Text

```rust
text::slugify("Charity Run München 2026!")  // "charity-run-münchen-2026"
text::strip_urls(&comment)                  // moderates links out of public free text
```

`slugify` keeps non-ASCII letters. Dropping them silently turns a German or
Turkish title into an empty slug, and transliteration needs a language to be
correct.

`strip_urls` removes links, including scheme-less ones (`www.example.com`),
from text shown to other users. Escaping a link renders it harmless to the
browser but still displays it, which is exactly what spam wants.

For escaping HTML outside a template, use `tera::escape_html` — the prelude
re-exports `tera`, so there is no reason to write another `&`/`<`/`>` chain.

## QR codes

```rust
let src = qr::svg_data_uri("https://example.com/ticket/42", 180);
```

Rendered as SVG and base64-encoded into a `data:` URI — the only form that
works in the two places a QR code is usually needed and an HTTP request is not
available: a generated PDF (whose renderer blocks every non-`data:`
subresource on purpose) and an HTML email.

`qr::SepaTransfer` builds an EPC069-12 GiroCode for invoices and donation
receipts. A transfer missing its IBAN or account holder returns `None`, because
a QR code that opens a banking app with no recipient is worse than none.

## HTML → PDF (`pdf` feature)

Invoices, receipts, certificates, tickets, reports.

```toml
full_stack_engine = { version = "9.1", features = ["pdf"] }
```

```rust
let bytes = pdf::render_template(&data, "invoices/receipt", &ctx, &PageSetup::A4).await?;
mail::send_mail_with_attachments(
    &data.config, to, "Your receipt", &html,
    vec![MailAttachment::pdf("receipt.pdf", bytes)],
).await?;
```

One Chromium instance is launched lazily and reused; renders are serialized so
memory stays bounded across a batch; each render is bounded by a timeout; a
failed render drops the instance so a dead browser doesn't poison later calls.

**Subresources are restricted to `data:` and `about:`.** This is why it belongs
in the framework: a document is often attacker-influenced (an uploaded
template, a user-supplied name), and Chromium will happily pull
`file:///etc/passwd` into an `<img>` or reach an internal address through CSS
`url()`. It's an allow-list, not a block-list — a block-list has to anticipate
every scheme Chromium supports.

`PageSetup::A4` is edge-to-edge, because a template built for print lays out
its own margins. Chromium's default is US Letter plus ~1 cm all round, which is
narrower and ~4 cm shorter than A4 — enough to push the end of a one-page
document onto a nearly empty second page.

Needs Chromium in the runtime image:

```dockerfile
RUN apt-get update && apt-get install -y chromium && rm -rf /var/lib/apt/lists/*
ENV CHROME=/usr/bin/chromium
```

## CORS

```bash
CORS_ALLOWED_ORIGINS=*                                        # public read API
CORS_ALLOWED_ORIGINS=https://app.example.com,http://localhost:3000
```

Unset means same-origin only, which is what an app without a public API wants.
`.cors(...)` in `lib.rs` sets the default; the variable overrides it, so access
can change without a deploy.

`*` echoes the requesting origin (with `Vary: Origin`, so caches stay correct)
and **never** permits credentials — allowing cookies from anywhere would mean
any site could read a logged-in user's data. A named origin list is an explicit
trust decision, so those may send the session cookie.

An entry without a scheme, or with a path, is a **boot error**. Both forms are
accepted by a naive parser and then match no browser, so CORS silently does
nothing — the worst outcome, because it looks configured.

For a different policy on one scope, `cors_middleware(&CorsConfig::Any)` can be
wrapped around a `web::scope`.

## API documentation

```rust
.api_docs(
    ApiDocs::new("Example API", env!("CARGO_PKG_VERSION"), "Public data.")
        .paths(services::api::openapi_paths())
        .schemas(services::api::openapi_schemas()),
)
```

Mounts `GET /api/openapi.json` and a browsable `GET /api/docs`. The
model-derived half comes from every `#[model(api)]` struct — column names,
types and nullability straight from the registry, so it cannot drift from what
the endpoints return. Hand-written routes are described next to their handlers
and merged over the generated ones, so a hand-written entry replaces a
generated path of the same name.

Both routes mount after the app's own, so claiming `/api/docs` in `configure`
replaces the page — the same override rule as everywhere else.

## First admin

```bash
ADMIN_EMAIL=admin@example.com
ADMIN_PASSWORD=...
```

When the auth module is installed and no account holds an admin role, one is
created at boot. A fresh deployment is therefore reachable without a
hand-written migration or a password hash committed to the repository.

It checks for *any* existing admin, not for this email: the question is "can
somebody administer this app", and re-running with a different address must not
quietly mint a second superuser. Safe to leave set permanently.

## Uploads

`uploads::save_upload` has a counterpart now:

```rust
uploads::delete_upload(&user.photo_path);
```

A path that does not name a file directly inside `uploads/` is ignored, never
followed — the stored path usually arrives from a database column, and a column
is only as trustworthy as everything that has ever written to it. Deletion is
best effort: a file already gone is `false`, not an error, because callers
delete while clearing the column that referenced it.

## Binaries and dev tooling

`src/bin/dev.rs`, the runner for `cargo run --bin dev`:

```rust
fn main() -> std::io::Result<()> {
    full_stack_engine::dev::run(env!("CARGO_PKG_NAME"))
}
```

Starts the theme's Astro dev server and the backend with hot reload, and stops
both together.

`--hash-password` is a flag on the app's own binary rather than a
`bin/hash_password.rs` every app copies — and it necessarily uses the same
parameters the app verifies with, which a separate tool can drift away from:

```bash
./myapp --hash-password 'hunter2'
```

`dev::preview_mail` renders one email template with sample data and real
translations, and optionally sends it. Config validation, locale layering,
theme loading and the SMTP transport are the framework's; the template list and
its sample data stay in the app. See the starter's `src/bin/test_email.rs`.

## Testing

`testing::load_themes_localized(themes, lang, currency)` parses the theme stack
with the filters bound the way boot binds them. Use it wherever a test renders
a real page: plain `load_themes` formats dates as ISO and amounts without a
symbol, which is not what the app will serve, so a page snapshot taken that way
would not catch a formatting regression.
