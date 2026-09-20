# Performance and hardening (framework 8)

What the framework does to every response, what it validates at boot, and the
two supply-chain checks that gate it. Written after measuring a real app, so the
numbers below are from `running-for-jesus-web` and the starter, not estimates.

## Responses

### Compression

`actix_web::middleware::Compress` is registered for every response, outside
`ErrorHandlers` so error pages are compressed too. Nothing was compressed
before: a 77 KB page went out as 77 KB even when the client asked for gzip.

### Asset caching

Theme assets under `_astro/` carry a content hash in the filename, so their
bytes can never change under the same URL. They are served
`Cache-Control: public, max-age=31536000, immutable`; everything else gets
`public, max-age=300, must-revalidate`.

Before this, a 44 KB stylesheet was re-downloaded in full on every navigation —
no `Cache-Control`, no `ETag`, nothing telling the browser it could keep the copy
it already had.

### The page payload

Every page's render context is serialised into `__fse-props__` for client-side
code. It used to include an `i18n` key holding **every** configured language's
full translation tree:

| | starter home | RFJ home |
|---|---|---|
| was, uncompressed | 33.5 KB | 77.4 KB |
| └ `i18n`, read by nothing | 16.3 KB | 40.4 KB |
| now, uncompressed | 18.7 KB | — |
| now, gzipped | **6.9 KB** | — |

`i18n` appeared in no Astro source and no built template. It is gone; `t` (the
request's language) stays. An app that needs another language on the client
should fetch it rather than ship it on every page.

`starter/tests/page_snapshots.rs` snapshots the payload's composition, so a
regression shows up as a diff in review.

## Content-Security-Policy

In production `script-src` names a per-request nonce instead of allowing
`'unsafe-inline'`. That is the difference between "an injected `<script>` runs"
and "an injected `<script>` is refused by the browser".

One middleware owns both halves — the policy header and the nonce in the body —
because a policy naming a nonce and a body carrying a different one is a broken
page. It applies to every HTML response: rendered pages, the themed error page,
and anything proxied from a theme dev server.

- `style-src` keeps `'unsafe-inline'`. A nonce cannot cover `style="..."`
  attributes (those are `style-src-attr`), and Astro and Tailwind both emit them.
- Development keeps `'unsafe-inline'` and `'unsafe-eval'`: HMR needs both, and
  dev-server pages bypass the renderer that stamps nonces.
- A handler that sets its own policy is never overwritten — the uploads mount
  serves user files under `sandbox`, and replacing that would be stored XSS.

`tests/hardening_http.rs` asserts the policy and the body agree, that nonces are
never reused, and that the uploads policy survives.

## Configuration

`config::Config::from_env()` reads and validates the whole environment once, at
boot, and reports **every** problem together:

```
invalid configuration (3 problems):
  - DOMAIN is not set
  - PROTOCOL is `htps`; expected `http` or `https`
  - SMTP is partially configured: SMTP_PASS missing. Set all three or none.
```

Two failure modes this removes:

- **One error per boot.** Each setting used to be an `expect` that panicked on
  the first missing variable, so a fresh deployment was fixed by rebooting once
  per mistake.
- **Failures discovered by users.** SMTP was read at *send* time, so a typo in
  `SMTP_HOST` passed boot, passed every test, and surfaced weeks later as a
  password reset that silently did nothing. `EMAIL_VERIFICATION_ENABLED=true`
  without working SMTP is now a boot error too, rather than a registration flow
  that strands every new user unverified.

Secrets are `secrecy::SecretString`: no `Debug`, no `Display`, so `JWT_SECRET`
and `SMTP_PASS` cannot reach a log line or a telemetry backend by accident.
Reading one is an explicit `.expose_secret()` that shows up in review.

## Health probes

| Route | Checks | Use |
|---|---|---|
| `GET /healthz` | nothing | liveness |
| `GET /readyz` | `SELECT 1` on the pool | readiness; `503` when it fails |
| `./app --healthcheck` | probes its own `/healthz`, exits 0/1 | Docker `HEALTHCHECK` |

Liveness deliberately touches nothing: a failing database should take the
instance out of rotation, not get the container killed and restarted, which fixes
nothing and drops the in-flight requests too.

The `--healthcheck` flag exists so the runtime image needs no `curl` — the image
is `debian-slim` plus `ca-certificates`, and an HTTP client carried solely to
answer `HEALTHCHECK` would be a package and its CVE stream for nothing.

Both probes log their access line at `debug`, not `info`; a probe every few
seconds would otherwise be most of a quiet service's log.

## Sessions

`AuthUser` used to run `SELECT sessions_valid_after` on **every** authenticated
request. That is now cached for 5 seconds (`moka`), which removes a pool
acquisition from the hot path of every request.

The cost is stated plainly: **every** write to `users.sessions_valid_after`, and
every user deletion, must be followed by
`auth::invalidate_session_cache(user_id)`, or revocation silently becomes
"revocation within 5 seconds". Prefer `auth::revoke_sessions(db, user_id)`, which
does both halves and cannot be half-called. The framework's five revocation
sites all do this.

### A hole closed on the way

Writing this cache surfaced a pre-existing flaw. Revocation stamps
`sessions_valid_after` with the current second, and acceptance was
`claims.iat >= cutoff` — so **any token minted during the same second as a
password reset, role change or email change survived it**. It is now `>`, which
revokes that second entirely.

At one-second granularity there is no way to distinguish "issued just before" from
"issued just after" within the cutoff second, so the trade is unavoidable: a token
minted in that same second is now rejected even if it was issued after the
revocation. That fails closed instead of open. In practice a user takes more than
a second to get back through `/logout` and `/login`; if it happens they log in
again and succeed.

## OpenAPI

`models::openapi::spec()` builds an OpenAPI 3.0 document from the model
registry — every `#[model(api)]` endpoint, with the right column names, types and
nullability, and no chance of drifting from what the endpoint returns.

It describes generated routes only. `starter/src/services/api.rs` shows the
pattern for an app that also hand-writes endpoints: generated paths as the base,
hand-written ones merged in. The starter's `/api/products` is an *override*
(published rows only, different parameters), so its real contract differs from the
generated one and is spelled out.

## Supply chain

`deny.toml` at the repo root gates three things:

```bash
cargo install cargo-deny
cargo deny --manifest-path framework/Cargo.toml check
cargo deny --manifest-path starter/Cargo.toml   check
```

Its first run found five real problems, now fixed:

| Finding | Resolution |
|---|---|
| `rustls` TLS 1.3 handshake acceptance | updated to 0.23.45 |
| `rustls-webpki` panic on malformed CRL | updated to 0.103.15 |
| `h2` 0.4 unbounded empty DATA frames | updated to 0.4.19 |
| `dotenv` unmaintained for years | swapped for `dotenvy` |
| `jsonwebtoken`'s `rsa` (Marvin timing attack) | switched to the `aws_lc_rs` backend; the framework signs HS256 only, and aws-lc-rs was already in the tree |

Two are recorded as documented exceptions rather than silently allowed:

- **`h2` 0.3** — same DoS, reachable only through `actix-http 3.x`, which pins
  `h2 ^0.3`. actix-web negotiates HTTP/2 only over TLS and these apps serve plain
  HTTP behind a terminating proxy, so the code path is never entered. Re-check if
  an app ever binds TLS directly.
- **`actix-governor` is GPL-3.0-or-later.** See below.

### TLS is rustls throughout

`sqlx` uses `tls-none` (SQLite is a local file, so it never opens a TLS
connection), `lettre` uses rustls with the bundled webpki roots, and reqwest 0.13
already defaults to rustls. Nothing in the tree links OpenSSL, which is why
`starter/Dockerfile` no longer installs `pkg-config`, `libssl-dev` or `openssl`.
`cargo deny`'s `bans` section keeps it that way.

### The licence conflict, resolved

`cargo deny`'s first run found that **`actix-governor` is GPL-3.0-or-later** (its
`LICENSE` is the full GPLv3), while this framework publishes as
`MIT OR Apache-2.0`. Linking it unconditionally for the site-wide rate limiter
made the combined work — and every binary built on the framework — a GPLv3
derivative, contradicting the declared licence.

Fixed in **9.0.0**: `src/rate_limiter.rs` is now Actix middleware written
directly over [`governor`](https://docs.rs/governor), which is **MIT**. Only the
Actix glue was ever GPL; the algorithm was always in `governor`, and the
framework already supplied its own key extraction, which was most of that glue.
The `deny.toml` exception is gone, so a GPL crate reappearing is a failing check
rather than a footnote.

What the replacement does, and what it has to get right:

- **Buckets are shared across workers.** Actix builds the middleware stack once
  per worker thread, so a limiter created per worker would multiply the real
  limit by the worker count. `shared_limiter` hands out an `Arc` from a
  process-wide cache keyed by call site *and* rate — one bucket per endpoint per
  client, however many workers there are. `one_call_site_yields_one_shared_limiter`
  pins this.
- **Exemption is server-decided.** `RequestKey::key` returns `Ok(None)` to skip
  limiting, and the only implementation that does so matches the request path
  against a list fixed at boot. Nothing a client sends can produce an exempt key.
- **Key extraction failure rejects the request.** If no client address can be
  determined the request gets a 500, not a free pass — failing open would turn a
  transport misconfiguration into an unmetered endpoint.
- **The key store is pruned.** A keyed limiter grows one entry per distinct
  address, forever, which a scanner rotating addresses would turn into a slow
  leak. `retain_recent` runs at most once a minute, on the request path, and only
  drops buckets indistinguishable from fresh — so pruning can never grant extra
  allowance.
- **429 carries `Retry-After`**, rounded up, so a client obeying it does not
  retry straight into another refusal. The body is plain text: a rejected request
  must not cost a template render, which is the work being shed.

Verified against a running server: the configured burst is honoured and the next
request is refused with `Retry-After`, `/api` stays unmetered, `POST /login` is
one per ten seconds, and a different `X-Forwarded-For` gets its own bucket.

**Versions 8.1.0 and earlier still carry the GPL dependency.** Upgrading to
9.0.0 is what resolves it for a downstream binary.
