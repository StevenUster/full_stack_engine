# Observability

Structured logging, per-request tracing, error reporting and OpenTelemetry
export. All of it is set up by `FrameworkApp::run()` — an app configures it with
environment variables and never calls an observability API directly.

The implementation is one module, [`framework/src/observability.rs`](../framework/src/observability.rs),
plus the error classification in [`framework/src/error.rs`](../framework/src/error.rs).

## The shape of it

```
  app + framework code ──► tracing::{info,warn,error,debug}
  sqlx / actix / lettre ─► log::*  ──(tracing-log bridge)──┤
                                                           ▼
                                                  tracing_subscriber
                                                           │
                           ┌───────────────────────────────┼───────────────┐
                           ▼                               ▼               ▼
                     fmt layer                       OTLP exporter    Sentry layer
                (stdout: pretty/compact/json)      (`otel` feature)  (`sentry` feature)
```

One front end, `tracing`. Application code records events and spans; which
backends those reach is a deployment decision. Turning a backend on or off never
changes code, and no handler ever calls a vendor's SDK.

`log` is still a dependency, because sqlx, actix-web, lettre and reqwest all
record through it — the `tracing-log` bridge funnels those into the same
subscriber. Existing app code written against `log::info!` keeps working
unchanged.

## Requests

`tracing-actix-web`'s `TracingLogger` (Actix's own middleware mechanism — no
Tower) opens one span per request and keeps it entered for the whole response,
including the body stream. Every log line written inside a handler therefore
carries that request's `request_id`, `http.route` and `trace_id` without the
handler doing anything.

The span's fields are built by the framework's own `FseRootSpan`, following
OpenTelemetry's HTTP server conventions:

| Field | Example |
|---|---|
| `http.request.method` | `GET` |
| `http.route` | `/admin/products/{id}` — the *pattern*, so span names stay low-cardinality; `unmatched` when nothing routed |
| `url.path` | `/admin/products/42` |
| `http.response.status_code` | `200` |
| `http.server.request.duration_ms` | `5.51` |
| `client.address` | proxy-aware, see below |
| `user_agent.original`, `server.address`, `network.protocol.version` | |
| `request_id` | uuid, also returned as the `x-request-id` response header |
| `trace_id` | present when telemetry is on |
| `exception.message` | the failure's full cause chain, on a failed request |
| `otel.kind` / `otel.name` / `otel.status_code` | read by the OTLP exporter |

Plus one access-log line per request, on the `full_stack_engine::access` target
(`RUST_LOG=full_stack_engine::access=off` silences it on its own). Its level
follows the status: `error` for 5xx, `info` otherwise — so a scanner's 404s
never compete with real failures, and `LOG_LEVEL=error` shows only what the
server got wrong.

### What is never recorded

Query strings, request and response headers (`Authorization`, `Cookie`,
`Set-Cookie`), request and response bodies, form fields. There is no allow-list
to get wrong: those values are never read.

This is not hypothetical. The auth module passes single-use credentials in query
strings (`/reset-password?token=…`, `/verify-email?token=…`), and
`tracing-actix-web`'s stock `DefaultRootSpanBuilder` records `http.target` from
`path_and_query()` — using it would have written every password-reset token to
stdout and forwarded it to every telemetry backend. That is the main reason
`FseRootSpan` exists; the second is that the stock builder takes the client IP
from `realip_remote_addr()`, which trusts the *left-most* `X-Forwarded-For`
entry and is therefore client-spoofable. `FseRootSpan` reuses the rate limiter's
resolver (right-most entry), so `client.address` and the rate-limit key are the
same address.

`tests/observability_http.rs` holds these as assertions: a request carrying a
reset token, a bearer token and a session cookie goes through the real
middleware and nothing it records may contain any of the three.

**Path parameters are recorded** as part of `url.path`. Do not put a secret in a
path segment.

## Errors

`AppError` splits into two groups, and `AppError::is_client_error()` is that
split:

- **Domain errors** (`NotFound`, `Auth`, `NoAuth`, `BadRequest`, `User`) —
  ordinary traffic. A 4xx, a message written for the person reading it, logged
  at `info`. A 404 is not an incident.
- **Internal errors** (`Db`, `Reqwest`, `Serde`, `Internal`, `Unexpected`) — a
  500, the user told nothing about the internals, the full cause chain logged at
  `error` and reported to the error backend.

A failure is logged **once**, by the request span, with its whole cause chain:

```
handler returns Err(AppError)
     │
     ▼
ResponseError::error_response()   attaches an `ErrorDetail` (cause chain, log-only)
     │
     ▼
ErrorHandlers → render_error_page()   renders the themed page, carrying the detail forward
     │
     ▼
FseRootSpan::on_request_end()     logs it once, at a severity taken from the status,
                                   and records it on the request's span
```

No call site logs its own error, so nothing is double-reported and no handler
has to remember to write a log line.

### Keeping the cause: `.context()`

`AppError::Internal(format!("…: {e}"))` flattens the cause into a string. Use
`ErrorContext` instead — it keeps the original error as `source()`, so the whole
chain is rendered and an error backend can group by root cause:

```rust
use full_stack_engine::prelude::*;

let raw = std::fs::read(path).context("reading the import file")?;
let parsed: Config = serde_json::from_str(&raw)
    .with_context(|| format!("parsing {path}"))?;
```

```
reading the import file: No such file or directory (os error 2)
```

`anyhow` is deliberately not used: `AppError` has to implement actix's
`ResponseError`, mapping every variant to a status code and a user-safe message,
which `anyhow::Error` cannot do. `thiserror` stays, and the one thing `anyhow` is
genuinely better at — context without loss — is covered by `AppError::Unexpected`
and `ErrorContext`.

### Panics

A panic hook reports through `tracing` (payload + location) before chaining to
whatever hook was installed before it, so a panic is visible in the logs and in
the error backend rather than being swallowed into a bare 500.

## Configuration

Nothing is required. Local development logs at `debug` in colourised multi-line
form; production logs at `info` as one JSON object per line and exports nothing
until an endpoint is configured.

| Variable | Default | Meaning |
|---|---|---|
| `RUST_LOG` | — | Full `tracing` directives. Appended last, so it overrides everything below. |
| `LOG_LEVEL` | `debug` (dev) / `info` (prod) | Level for the app's own code. |
| `LOG_FORMAT` | `pretty` (dev) / `json` (prod) | `pretty`, `compact` or `json`. |
| `SERVICE_NAME` | `FrameworkApp::service_name`, else the executable name | `service.name`. |
| `SERVICE_VERSION` | `FrameworkApp::service_version`, else `unknown` | `service.version`. |
| `DEPLOY_ENV` | `development` / `production` | `deployment.environment.name`. |
| `TELEMETRY_ENABLED` | on iff an OTLP endpoint is set | Hard switch for span export. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | — | Base URL; `/v1/traces` is appended. Setting it turns export on. |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | — | Exact traces endpoint; wins over the above. |
| `OTEL_EXPORTER_OTLP_HEADERS` | — | e.g. `authorization=Bearer%20token` for a hosted backend. |
| `TELEMETRY_SAMPLE_RATIO` | `1.0` | Head sampling for traces with no parent. |
| `SENTRY_DSN` | — | Enables error/panic reporting when set. |

A malformed value never stops the app from booting: an unparseable `RUST_LOG`
falls back to `info`, an unknown `LOG_FORMAT` falls back to the default, and an
unreachable OTLP collector leaves the app running with logs only.

Noisy dependencies are quieted by default (`sqlx=warn` — it logs the text of
every statement at `info` — plus `h2`, `hyper`, `reqwest`, `rustls`, `lettre`,
`mio`). `RUST_LOG` is appended after those defaults, so `RUST_LOG=sqlx=debug`
turns query logging back on.

`.env` is read **before** the subscriber is built, so logging settings kept there
take effect. (They previously did not: the logger was initialised first, which
silently ignored `RUST_LOG` in `.env`.)

## Cargo features

Both are off by default — an app that wants neither pays for neither.

```toml
full_stack_engine = { version = "6", features = ["otel"] }          # OTLP export
full_stack_engine = { version = "6", features = ["otel", "sentry"] } # + error reporting
```

- **`otel`** — OTLP span export over **HTTP/protobuf**, reusing the `reqwest`
  already in the tree. Deliberately not `grpc-tonic`, which would pull tonic and
  therefore Tower into an Actix app.
- **`sentry`** — errors and panics to any Sentry-protocol backend (Sentry,
  GlitchTip, self-hosted), wired in as a `tracing` layer: `ERROR` events become
  issues, `WARN` and below become breadcrumbs for context. Spans are not sent as
  Sentry transactions — tracing is OTLP's job, and duplicating it would double
  the egress for no new information. `send_default_pii` stays off.

The starter enables `otel`. It stays dormant until an endpoint is set.

## Running it locally

Point at any OTLP-compatible backend — an OpenTelemetry Collector, Tempo,
Jaeger, Honeycomb, Datadog's OTLP intake:

```bash
docker run -p 4318:4318 -p 16686:16686 jaegertracing/all-in-one:latest
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 cargo run
# traces at http://localhost:16686
```

Useful combinations:

```bash
LOG_FORMAT=json cargo run                            # see what prod will emit
RUST_LOG=sqlx=debug cargo run                        # every SQL statement
RUST_LOG=full_stack_engine::access=off cargo run     # drop the access log
LOG_LEVEL=error cargo run                            # only what the server got wrong
```

## In production

- **Logs**: `json` to stdout, collected by the container runtime. Nothing writes
  to a file (cron job run logs excepted — see `cron::LogRotation`).
- **Set `SERVICE_VERSION`** to the deployed commit SHA. Two builds of the same
  crate version are not the same binary, and this is what ties an error to a
  release.
- **Lower `TELEMETRY_SAMPLE_RATIO`** on a busy service. Sampling is
  parent-based, so a trace a caller already decided to sample is recorded
  whatever the local ratio says — a distributed trace is never stitched together
  from half the services it passed through.
- **Cost of it when off**: one uuid and one span per request. Spans are only
  exported when an endpoint is configured, and the batch exporter runs on its own
  thread, so export never blocks a request.
- **Shutdown** flushes the exporter's last batch, so spans from the final second
  are not lost.

## Correlating a user report

A user quotes the `x-request-id` from their response (it is also in the error
page's render context, as `request_id`, for a theme to display). That id is
`request_id` on the request's span and on every log line the request produced:

```bash
jq 'select(.spans[]?.request_id == "ec7cd01f-…")' app.log
```

Under `otel`, the same request's `trace_id` links straight to the trace.

## Instrumenting app code

The prelude's `debug!`/`info!`/`warn!`/`error!` are `tracing`'s macros. The call
syntax is unchanged, and they now also take structured fields:

```rust
use full_stack_engine::prelude::*;

#[instrument(skip(data), fields(order.id = %id))]
async fn place_order(data: Data<AppData>, id: i64) -> AppResult {
    info!(order.id = id, "order placed");
    // ...
}
```

`#[instrument]` adds the handler's own span as a child of the request span — it
shows up nested in a trace, and everything it logs inherits both sets of fields.
Skip large arguments (`skip(data)`) and never put a secret in a field.
