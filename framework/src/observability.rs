//! Logging, request tracing and error reporting — the whole observability
//! stack in one place.
//!
//! # Shape
//!
//! Everything the app emits goes through **one** front end, [`tracing`]:
//!
//! ```text
//!   app + framework code ──► tracing::{info,warn,error,...}
//!   sqlx / actix / lettre ─► log::*  ──(tracing-log bridge)──┤
//!                                                            ▼
//!                                                   tracing_subscriber
//!                                                            │
//!                            ┌───────────────────────────────┼───────────────┐
//!                            ▼                               ▼               ▼
//!                      fmt layer                       OTLP exporter    Sentry layer
//!                 (stdout: pretty/compact/json)      (`otel` feature)  (`sentry` feature)
//! ```
//!
//! The consequence worth remembering: application code never talks to a
//! vendor. It records `tracing` events and spans; which backends those reach
//! is a deployment decision made by environment variables, and turning one
//! off changes no code.
//!
//! # Requests
//!
//! [`TracingLogger`](tracing_actix_web::TracingLogger) — Actix's own
//! middleware mechanism, not Tower — opens one span per request and keeps it
//! entered for the whole response, including the body stream. Every log line
//! produced while handling a request therefore carries that request's
//! `request_id`, and under the `otel` feature the span is also an OTLP server
//! span with the incoming `traceparent` as its parent.
//!
//! The span's fields are built by [`FseRootSpan`] rather than
//! `tracing-actix-web`'s `DefaultRootSpanBuilder`, for two concrete reasons:
//!
//! 1. The default builder records `http.target` from
//!    `uri().path_and_query()`, i.e. **including the query string**. This
//!    framework puts single-use credentials in query strings
//!    (`/reset-password?token=...`, `/verify-email?token=...`), so that field
//!    would write password-reset tokens to the log and to every telemetry
//!    backend. [`FseRootSpan`] records the path only.
//! 2. The default builder takes the client IP from
//!    `realip_remote_addr()`, which trusts the *left-most* `X-Forwarded-For`
//!    entry and is therefore client-spoofable. [`FseRootSpan`] reuses
//!    [`crate::rate_limiter::client_ip`], which reads the right-most entry.
//!
//! # What is deliberately never recorded
//!
//! Query strings, request or response headers (`Authorization`, `Cookie`,
//! `Set-Cookie`), request or response bodies, form fields. There is no
//! allow-list to get wrong: those values are never read in the first place.
//! Path *parameters* are recorded as part of `url.path`, so an app must not
//! put a secret in a path segment.

use std::time::Instant;

use actix_web::{
    Error, HttpMessage,
    body::MessageBody,
    dev::{ServiceRequest, ServiceResponse},
    http::StatusCode,
};
use tracing::{Span, field::Empty};
use tracing_actix_web::{RequestId, RootSpanBuilder};
use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt};

use crate::Env;

/// Target of the one-line-per-request access log. Split out so it can be
/// silenced on its own (`RUST_LOG=full_stack_engine::access=off`) without
/// touching the rest of the application's logging.
pub const ACCESS_TARGET: &str = "full_stack_engine::access";

/// Noisy dependencies, quieted unless the operator asks for them. Each is a
/// crate that logs per-query or per-connection at `info`/`debug`; at the
/// framework's own default level they would bury the application's own
/// events. Anything in `RUST_LOG` overrides these, because it is appended
/// after them.
const DEPENDENCY_DEFAULTS: &[&str] = &[
    // Logs the text of every statement it executes, at INFO.
    "sqlx=warn",
    "actix_server=info",
    "actix_http=warn",
    "h2=warn",
    "hyper=warn",
    "hyper_util=warn",
    "reqwest=warn",
    "rustls=warn",
    "lettre=warn",
    "mio=warn",
    "want=warn",
];

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Configuration
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// How log lines are written to stdout.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LogFormat {
    /// Multi-line, colourised, one field per line. The dev default: readable
    /// in a terminal, useless to a log shipper.
    Pretty,
    /// One line per event, human-readable.
    Compact,
    /// One JSON object per line, with the span fields attached. The prod
    /// default: what Loki/Elastic/CloudWatch want.
    Json,
}

impl LogFormat {
    /// Parses `LOG_FORMAT`. An unrecognised value falls back to `default`
    /// rather than failing the boot — a typo in a log setting must not stop
    /// an app from starting.
    fn parse(value: Option<&str>, default: Self) -> Self {
        match value.map(str::trim) {
            Some("pretty") => Self::Pretty,
            Some("compact") => Self::Compact,
            Some("json") => Self::Json,
            _ => default,
        }
    }
}

/// Everything the observability stack reads from the environment, resolved
/// once at boot. Kept as a plain struct with a pure [`Settings::from_env`] so
/// the precedence rules are unit-testable.
///
/// | Variable | Default | Meaning |
/// |---|---|---|
/// | `RUST_LOG` | — | Full `tracing` filter directives. Wins over `LOG_LEVEL`. |
/// | `LOG_LEVEL` | `debug` (dev) / `info` (prod) | Level for the app's own code. |
/// | `LOG_FORMAT` | `pretty` (dev) / `json` (prod) | `pretty`, `compact` or `json`. |
/// | `SERVICE_NAME` | executable file name | `service.name` on every span. |
/// | `SERVICE_VERSION` | `unknown` | `service.version`; set to the release/commit. |
/// | `DEPLOY_ENV` | `development` / `production` | `deployment.environment.name`. |
/// | `TELEMETRY_ENABLED` | on iff an OTLP endpoint is set | Master switch for span export. |
/// | `OTEL_EXPORTER_OTLP_ENDPOINT` | — | Standard OTLP endpoint (`…/v1/traces` is appended). |
/// | `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | — | Exact traces endpoint; wins over the above. |
/// | `OTEL_EXPORTER_OTLP_HEADERS` | — | e.g. `authorization=Bearer x` for a hosted backend. |
/// | `TELEMETRY_SAMPLE_RATIO` | `1.0` | Head sampling ratio for traces with no parent. |
/// | `SENTRY_DSN` | — | Enables error/panic reporting when set. |
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// `tracing` filter directives, already merged (see
    /// [`Settings::filter_directives`]).
    pub filter: String,
    pub format: LogFormat,
    pub service_name: String,
    pub service_version: String,
    pub deploy_env: String,
    /// Whether to build an OTLP exporter. Always `false` without the `otel`
    /// feature.
    pub telemetry_enabled: bool,
    /// The traces endpoint, if one was configured explicitly. `None` lets the
    /// `OpenTelemetry` SDK apply its own defaults (`http://localhost:4318`).
    pub otlp_endpoint: Option<String>,
    /// Head sampling ratio in `0.0..=1.0`, applied only to traces that arrive
    /// without a parent — a sampled incoming `traceparent` is always honoured,
    /// so a distributed trace never ends up half-recorded.
    pub sample_ratio: f64,
    /// Sentry-protocol DSN, if error reporting is configured.
    pub sentry_dsn: Option<String>,
}

impl Settings {
    /// Reads the environment. `env` decides the development-vs-production
    /// defaults; nothing else differs between the two.
    ///
    /// Call this *after* the `.env` file has been loaded, or values set there
    /// are invisible.
    #[must_use]
    pub fn from_env(env: Env) -> Self {
        let dev = env == Env::Dev;
        let var = |key: &str| {
            std::env::var(key)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };

        let otlp_endpoint = var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
            .or_else(|| var("OTEL_EXPORTER_OTLP_ENDPOINT"));

        Self {
            filter: Self::filter_directives(
                var("RUST_LOG").as_deref(),
                var("LOG_LEVEL").as_deref(),
                env,
            ),
            format: LogFormat::parse(
                var("LOG_FORMAT").as_deref(),
                if dev {
                    LogFormat::Pretty
                } else {
                    LogFormat::Json
                },
            ),
            service_name: var("SERVICE_NAME").unwrap_or_else(default_service_name),
            service_version: var("SERVICE_VERSION").unwrap_or_else(|| "unknown".to_string()),
            deploy_env: var("DEPLOY_ENV")
                .unwrap_or_else(|| if dev { "development" } else { "production" }.to_string()),
            // Exporting spans nowhere is the only sane default, and an
            // endpoint is the clearest signal that a backend exists. An
            // explicit `TELEMETRY_ENABLED=false` still wins, so telemetry can
            // be switched off without unsetting the endpoint.
            telemetry_enabled: cfg!(feature = "otel")
                && match var("TELEMETRY_ENABLED").as_deref() {
                    Some("true" | "1") => true,
                    Some(_) => false,
                    None => otlp_endpoint.is_some(),
                },
            otlp_endpoint,
            sample_ratio: var("TELEMETRY_SAMPLE_RATIO")
                .and_then(|v| v.parse::<f64>().ok())
                .map_or(1.0, |r| r.clamp(0.0, 1.0)),
            sentry_dsn: var("SENTRY_DSN"),
        }
    }

    /// The filter string handed to [`EnvFilter`].
    ///
    /// Built as `<dependency defaults>,<level for our own code>,<RUST_LOG>` —
    /// later directives win in `tracing`, so an operator's `RUST_LOG` can
    /// always override any default here, including turning a quieted
    /// dependency back on (`RUST_LOG=sqlx=debug`).
    fn filter_directives(rust_log: Option<&str>, log_level: Option<&str>, env: Env) -> String {
        let level = log_level.unwrap_or(if env == Env::Dev { "debug" } else { "info" });
        let mut directives = DEPENDENCY_DEFAULTS.join(",");
        directives.push(',');
        directives.push_str(level);
        if let Some(rust_log) = rust_log {
            directives.push(',');
            directives.push_str(rust_log);
        }
        directives
    }
}

/// The executable's file name — a better `service.name` default than a fixed
/// string, since it is already the app's name in practice.
fn default_service_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "fse-app".to_string())
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Initialisation
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Keeps the telemetry pipeline alive. Dropping it flushes whatever the batch
/// exporter still holds, so spans from the last second before shutdown are not
/// lost; without the `otel` feature it does nothing.
///
/// Hold it for as long as the process serves traffic —
/// [`crate::FrameworkApp::run`] keeps it until the server stops.
#[must_use = "dropping the guard immediately shuts telemetry down again"]
pub struct Guard {
    #[cfg(feature = "otel")]
    tracer_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
    #[cfg(feature = "sentry")]
    _sentry: Option<sentry::ClientInitGuard>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        #[cfg(feature = "otel")]
        if let Some(provider) = self.tracer_provider.take() {
            // Best-effort: a collector that is already gone must not turn a
            // clean shutdown into a failure.
            if let Err(err) = provider.shutdown() {
                eprintln!("telemetry shutdown failed: {err}");
            }
        }
    }
}

/// Installs the global subscriber, the panic hook and (when configured) the
/// OTLP exporter and Sentry client.
///
/// Call exactly once, as early as possible and after the `.env` file has been
/// loaded. A second call is a no-op apart from returning a fresh guard: the
/// global subscriber can only be set once, and re-installing it would panic.
///
/// # Panics
///
/// Never. A telemetry backend that cannot be reached is reported on stderr and
/// the app keeps running with plain logging: observability must not be able to
/// take an application down.
pub fn init(settings: &Settings) -> Guard {
    let filter = EnvFilter::builder()
        .parse(&settings.filter)
        // `Settings::filter_directives` can only be wrong if `RUST_LOG` is,
        // and a malformed `RUST_LOG` must not stop the app from booting.
        .unwrap_or_else(|err| {
            eprintln!("invalid log filter ({err}); falling back to `info`");
            EnvFilter::new("info")
        });

    #[cfg(feature = "sentry")]
    let sentry_guard = init_sentry(settings);

    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer(settings));

    #[cfg(feature = "sentry")]
    let registry = registry.with(sentry_tracing_layer());

    #[cfg(feature = "otel")]
    let (registry, tracer_provider) = {
        let (layer, provider) = otel_layer(settings);
        (registry.with(layer), provider)
    };

    // `try_init` rather than `init`: a test binary or an embedding app may
    // already have a subscriber, and that is not a reason to abort.
    if registry.try_init().is_ok() {
        install_panic_hook();
    }

    tracing::info!(
        service.name = %settings.service_name,
        service.version = %settings.service_version,
        deployment.environment.name = %settings.deploy_env,
        log.format = ?settings.format,
        telemetry.enabled = settings.telemetry_enabled,
        error_reporting.enabled = settings.sentry_dsn.is_some(),
        "observability initialised"
    );

    Guard {
        #[cfg(feature = "otel")]
        tracer_provider,
        #[cfg(feature = "sentry")]
        _sentry: sentry_guard,
    }
}

/// The stdout layer, boxed because the three formats are three distinct types.
fn fmt_layer<S>(settings: &Settings) -> Box<dyn Layer<S> + Send + Sync>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    let layer = tracing_subscriber::fmt::layer();
    match settings.format {
        LogFormat::Pretty => Box::new(layer.pretty()),
        LogFormat::Compact => Box::new(layer.compact()),
        LogFormat::Json => Box::new(
            layer
                .json()
                // Without this the span fields — request_id, http.route,
                // client.address — are dropped from event records, which is
                // most of the value of having spans at all. The span *list*
                // rather than `with_current_span`: it already ends with the
                // innermost span, so the pair would write every field of a
                // request's span twice on every line.
                .with_span_list(true),
        ),
    }
}

/// Reports panics through `tracing` (and therefore to every configured
/// backend) before handing over to whatever hook was installed before —
/// Sentry's panic integration, or the default one that prints to stderr.
///
/// Actix already keeps a panicking handler from taking the worker down; this
/// is about the panic being *visible* rather than swallowed into a bare 500.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(std::string::ToString::to_string)
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        tracing::error!(
            panic.payload = %payload,
            panic.location = %info.location().map_or_else(|| "unknown".to_string(), ToString::to_string),
            "thread panicked"
        );
        previous(info);
    }));
}

#[cfg(feature = "otel")]
fn otel_layer<S>(
    settings: &Settings,
) -> (
    Option<Box<dyn Layer<S> + Send + Sync>>,
    Option<opentelemetry_sdk::trace::SdkTracerProvider>,
)
where
    // `Send + Sync` because `OpenTelemetryLayer` carries a `PhantomData<S>`,
    // and the returned layer is boxed as `dyn Layer<S> + Send + Sync`.
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a> + Send + Sync,
{
    use opentelemetry::{KeyValue, trace::TracerProvider as _};
    use opentelemetry_otlp::WithExportConfig as _;
    use opentelemetry_sdk::{
        Resource,
        propagation::TraceContextPropagator,
        trace::{Sampler, SdkTracerProvider},
    };

    if !settings.telemetry_enabled {
        return (None, None);
    }

    let mut builder = opentelemetry_otlp::SpanExporter::builder().with_http();
    if let Some(endpoint) = &settings.otlp_endpoint {
        builder = builder.with_endpoint(traces_endpoint(endpoint));
    }
    let exporter = match builder.build() {
        Ok(exporter) => exporter,
        Err(err) => {
            // Keep serving with logs only rather than refusing to boot
            // because a collector is misconfigured.
            eprintln!("OTLP exporter unavailable, telemetry disabled: {err}");
            return (None, None);
        }
    };

    let resource = Resource::builder()
        .with_service_name(settings.service_name.clone())
        .with_attributes([
            KeyValue::new("service.version", settings.service_version.clone()),
            KeyValue::new("deployment.environment.name", settings.deploy_env.clone()),
        ])
        .build();

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        // Parent-based: if an upstream service decided to sample this trace,
        // record it whatever the local ratio says, so a trace is never
        // stitched together from half the services it passed through.
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            settings.sample_ratio,
        ))))
        .build();

    // W3C `traceparent`, the interchange format every OTLP backend speaks.
    // Set globally so `FseRootSpan` can extract an incoming context and
    // outbound instrumentation can inject one.
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    let tracer = provider.tracer("full_stack_engine");
    opentelemetry::global::set_tracer_provider(provider.clone());

    (
        Some(Box::new(tracing_opentelemetry::layer().with_tracer(tracer))),
        Some(provider),
    )
}

/// OTLP/HTTP wants the full signal path. `OTEL_EXPORTER_OTLP_ENDPOINT` is a
/// base URL by specification, so append `/v1/traces` unless the operator
/// already pointed at a signal-specific path.
#[cfg(feature = "otel")]
fn traces_endpoint(endpoint: &str) -> String {
    let trimmed = endpoint.trim_end_matches('/');
    if trimmed.ends_with("/v1/traces") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1/traces")
    }
}

#[cfg(feature = "sentry")]
// `sentry::ClientOptions` is `#[non_exhaustive]`, so it cannot be built with a
// struct expression from outside its own crate — the fields have to be
// assigned onto a default.
#[allow(clippy::field_reassign_with_default)]
fn init_sentry(settings: &Settings) -> Option<sentry::ClientInitGuard> {
    let dsn = settings.sentry_dsn.clone()?;

    let mut options = sentry::ClientOptions::default();
    // Groups issues by deploy, and tells "this regressed in v1.4.2" apart from
    // "this has always been broken".
    options.release = Some(settings.service_version.clone().into());
    options.environment = Some(settings.deploy_env.clone().into());
    // Off by default and left off: the SDK's "default PII" includes the
    // request's IP address and user headers, none of which this framework has
    // decided to send to a third party.
    options.send_default_pii = false;
    options.attach_stacktrace = true;

    Some(sentry::init((dsn, options)))
}

/// Maps `tracing` severities onto Sentry: an `ERROR` event becomes an issue,
/// `WARN` and below become breadcrumbs that give that issue context. Spans are
/// not turned into Sentry transactions — tracing is OTLP's job here, and
/// duplicating it would double the egress for no new information.
#[cfg(feature = "sentry")]
fn sentry_tracing_layer<S>() -> sentry::integrations::tracing::SentryLayer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use sentry::integrations::tracing::EventFilter;

    sentry::integrations::tracing::layer().event_filter(|meta| match *meta.level() {
        tracing::Level::ERROR => EventFilter::Event,
        tracing::Level::TRACE => EventFilter::Ignore,
        _ => EventFilter::Breadcrumb,
    })
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Error detail
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Why a request failed, in the form logs and telemetry want it.
///
/// [`crate::error::AppError`] attaches this to its response;
/// [`FseRootSpan::on_request_end`] reads it, which is how a failed request is
/// logged exactly once, with its whole cause chain, at a severity taken from
/// the status. The error page reads it too, to show the detailed message in
/// development.
///
/// The user-facing counterpart is [`crate::error::AppError::user_message`],
/// kept apart on purpose: this one may name tables, hosts and file paths and
/// must never reach a response body.
#[derive(Clone, Debug)]
pub struct ErrorDetail {
    /// The error and every `source()` behind it, `": "`-joined.
    pub log_message: String,
    /// The status the error asked for, which is not always the status finally
    /// sent — the error page rewrites 401/403 to 404, so an unauthorised probe
    /// cannot tell a forbidden resource from a missing one.
    pub status: StatusCode,
}

impl ErrorDetail {
    /// Builds a detail from an error, following its `source()` chain.
    #[must_use]
    pub fn from_error(error: &dyn std::error::Error, status: StatusCode) -> Self {
        Self {
            log_message: source_chain(error),
            status,
        }
    }

    /// Builds a detail from something that can only be displayed.
    ///
    /// `actix_web::Error`'s `ResponseError` is `Debug + Display` but not
    /// `std::error::Error`, so for actix's own errors (a malformed form body, a
    /// rejected extractor) there is no chain to walk — the message is all
    /// there is.
    #[must_use]
    pub fn from_display(error: &impl std::fmt::Display, status: StatusCode) -> Self {
        Self {
            log_message: error.to_string(),
            status,
        }
    }
}

/// Renders `error` together with everything behind it: `"context: cause:
/// root cause"`.
///
/// This is why the framework's internal errors keep a `#[source]` instead of
/// being flattened into a `String` — the chain is the part that says *why*,
/// and a formatted message throws it away.
#[must_use]
pub fn source_chain(error: &dyn std::error::Error) -> String {
    let mut out = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let rendered = cause.to_string();
        // thiserror's `#[error("... {0}")]` already embeds the cause's
        // message; repeating it adds length and no information.
        if !out.contains(&rendered) {
            out.push_str(": ");
            out.push_str(&rendered);
        }
        source = cause.source();
    }
    out
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// The request span
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Wall-clock start of a request, stashed in its extensions so
/// [`FseRootSpan::on_request_end`] can report a duration.
struct StartedAt(Instant);

/// The framework's root span: OpenTelemetry HTTP semantic conventions, minus
/// every field that could carry a secret.
///
/// See the [module docs](self) for why this exists instead of
/// `DefaultRootSpanBuilder`, and for the list of what is never recorded.
pub struct FseRootSpan;

impl RootSpanBuilder for FseRootSpan {
    fn on_request_start(request: &ServiceRequest) -> Span {
        request.extensions_mut().insert(StartedAt(Instant::now()));

        let request_id = request
            .extensions()
            .get::<RequestId>()
            .map_or_else(|| "unknown".to_string(), ToString::to_string);

        let span = tracing::info_span!(
            "http_request",
            // ── OpenTelemetry HTTP server conventions ──
            http.request.method = %request.method(),
            // Path only. `path_and_query()` would leak the single-use tokens
            // this framework passes in query strings.
            url.path = %request.uri().path(),
            client.address = %client_address(request),
            user_agent.original = %header(request, "user-agent"),
            network.protocol.version = %protocol_version(request.version()),
            server.address = %header(request, "host"),
            // Filled in `on_request_end`, once routing has picked a pattern
            // and the response has a status.
            http.route = Empty,
            http.response.status_code = Empty,
            http.server.request.duration_ms = Empty,
            error.type = Empty,
            exception.message = Empty,
            // ── Correlation ──
            request_id = %request_id,
            trace_id = Empty,
            // ── Read by tracing-opentelemetry, ignored otherwise ──
            otel.name = Empty,
            otel.kind = "server",
            otel.status_code = Empty,
        );

        set_otel_parent(request, &span);
        span
    }

    fn on_request_end<B: MessageBody>(span: Span, outcome: &Result<ServiceResponse<B>, Error>) {
        let (status, route, detail) = match outcome {
            Ok(response) => (
                response.status(),
                response.request().match_pattern(),
                // The error page rewrites the response, so prefer the detail
                // it carried forward over the raw attached error.
                response
                    .response()
                    .extensions()
                    .get::<ErrorDetail>()
                    .cloned()
                    .or_else(|| {
                        response
                            .response()
                            .error()
                            .map(|err| ErrorDetail::from_display(err, response.status()))
                    }),
            ),
            Err(error) => {
                let status = error.as_response_error().status_code();
                (status, None, Some(ErrorDetail::from_display(error, status)))
            }
        };

        // A route that never matched gets one shared label rather than the raw
        // path, so a scanner probing random URLs cannot mint a span name per
        // probe.
        let route = route.unwrap_or_else(|| "unmatched".to_string());
        // Only the `Ok` arm still has the request, and therefore the start
        // instant; a middleware-level `Err` reports no duration rather than a
        // made-up one.
        let elapsed = outcome
            .as_ref()
            .ok()
            .and_then(|r| {
                r.request()
                    .extensions()
                    .get::<StartedAt>()
                    .map(|s| s.0.elapsed())
            })
            .map(|d| d.as_secs_f64() * 1000.0);

        span.record("http.route", tracing::field::display(&route));
        span.record("http.response.status_code", status.as_u16());
        span.record(
            "otel.name",
            tracing::field::display(format_args!("{} {route}", outcome_method(outcome))),
        );
        if let Some(ms) = elapsed {
            span.record("http.server.request.duration_ms", ms);
        }

        // A 5xx is the server's fault and an error for OTel; a 4xx is the
        // client's and must not colour the span red, or every 404 from a bot
        // would look like an outage.
        if status.is_server_error() {
            span.record("otel.status_code", "ERROR");
        } else {
            span.record("otel.status_code", "OK");
        }

        if let Some(detail) = &detail {
            span.record("error.type", tracing::field::display(status.as_u16()));
            span.record(
                "exception.message",
                tracing::field::display(&detail.log_message),
            );
        }

        emit_access_event(status, &route, elapsed, detail.as_ref());
    }
}

/// The single log line per request. `target` is [`ACCESS_TARGET`] so it can be
/// silenced independently; level follows the status code, because an error the
/// server caused should be findable with `LOG_LEVEL=error` while routine 404s
/// should not compete with it.
fn emit_access_event(
    status: StatusCode,
    route: &str,
    duration_ms: Option<f64>,
    detail: Option<&ErrorDetail>,
) {
    // Field sets must be identical across the arms, so `cause` is always
    // present — empty when there was no error.
    let cause = detail.map_or("", |d| d.log_message.as_str());
    let duration_ms = duration_ms.unwrap_or_default();
    let status = status.as_u16();

    // Plain field names rather than the span's dotted OTel ones: a dotted
    // name after `target:` is ambiguous to the macro parser, and the
    // convention names belong on the span, which is what a telemetry backend
    // reads. This line is for a human reading a terminal or grepping a file.
    if status >= 500 {
        tracing::error!(
            target: ACCESS_TARGET,
            route = %route,
            status,
            duration_ms,
            cause = %cause,
            "request failed"
        );
    } else if status >= 400 {
        // Client errors are expected traffic — informative, not a warning.
        tracing::info!(
            target: ACCESS_TARGET,
            route = %route,
            status,
            duration_ms,
            cause = %cause,
            "request rejected"
        );
    } else {
        tracing::info!(
            target: ACCESS_TARGET,
            route = %route,
            status,
            duration_ms,
            cause = %cause,
            "request completed"
        );
    }
}

fn outcome_method<B>(outcome: &Result<ServiceResponse<B>, Error>) -> String {
    outcome
        .as_ref()
        .ok()
        .map_or_else(|| "HTTP".to_string(), |r| r.request().method().to_string())
}

/// The version as OpenTelemetry spells it: `"1.1"`, not `"HTTP/1.1"`.
fn protocol_version(version: actix_web::http::Version) -> &'static str {
    match version {
        actix_web::http::Version::HTTP_09 => "0.9",
        actix_web::http::Version::HTTP_10 => "1.0",
        actix_web::http::Version::HTTP_11 => "1.1",
        actix_web::http::Version::HTTP_2 => "2",
        actix_web::http::Version::HTTP_3 => "3",
        _ => "unknown",
    }
}

/// Header value as a string, or `""` — never the header's raw bytes, and only
/// ever called for the two headers above (`user-agent`, `host`). No call site
/// reads `authorization`, `cookie` or `set-cookie`.
fn header(request: &ServiceRequest, name: &str) -> String {
    request
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// The client address, resolved the same proxy-aware way the rate limiter
/// resolves it (right-most `X-Forwarded-For`, so a prepended spoofed value is
/// ignored) — one definition of "who is calling" across the framework.
fn client_address(request: &ServiceRequest) -> String {
    crate::rate_limiter::client_ip(request)
        .map(|ip| ip.to_string())
        .unwrap_or_default()
}

/// Adopts an incoming W3C `traceparent` as the span's parent, so a request
/// that arrives from an already-traced caller continues that trace instead of
/// starting a new one. A no-op without the `otel` feature.
#[cfg(feature = "otel")]
fn set_otel_parent(request: &ServiceRequest, span: &Span) {
    use opentelemetry::trace::TraceContextExt as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    let parent = opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.extract(&HeaderExtractor(request.headers()))
    });
    let _ = span.set_parent(parent);
    // Recorded so plain log lines can be correlated with the trace in the
    // telemetry backend without leaving the terminal. Left unset when there is
    // no tracer installed: the all-zero id the SDK returns then is not a
    // missing value in a log search, it is an id that matches every request.
    let span_context = span.context().span().span_context().clone();
    if span_context.is_valid() {
        let trace_id = span_context.trace_id();
        span.record(
            "trace_id",
            tracing::field::display(format_args!("{trace_id:032x}")),
        );
    }
}

#[cfg(not(feature = "otel"))]
#[allow(clippy::needless_pass_by_value, clippy::missing_const_for_fn)]
fn set_otel_parent(_request: &ServiceRequest, _span: &Span) {}

/// Reads propagation headers for the `OpenTelemetry` extractor. Only keys the
/// propagator
/// asks for (`traceparent`, `tracestate`) are ever looked up.
#[cfg(feature = "otel")]
struct HeaderExtractor<'a>(&'a actix_web::http::header::HeaderMap);

#[cfg(feature = "otel")]
impl opentelemetry::propagation::Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0
            .keys()
            .map(actix_web::http::header::HeaderName::as_str)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_format_defaults_per_environment_and_tolerates_typos() {
        assert_eq!(
            LogFormat::parse(Some("json"), LogFormat::Pretty),
            LogFormat::Json
        );
        assert_eq!(
            LogFormat::parse(Some("compact"), LogFormat::Json),
            LogFormat::Compact
        );
        assert_eq!(
            LogFormat::parse(Some("pretty"), LogFormat::Json),
            LogFormat::Pretty
        );
        // Unset or misspelled falls back instead of failing the boot.
        assert_eq!(LogFormat::parse(None, LogFormat::Json), LogFormat::Json);
        assert_eq!(
            LogFormat::parse(Some("JSON"), LogFormat::Pretty),
            LogFormat::Pretty
        );
        assert_eq!(
            LogFormat::parse(Some(""), LogFormat::Pretty),
            LogFormat::Pretty
        );
    }

    #[test]
    fn filter_puts_rust_log_last_so_it_always_wins() {
        let filter = Settings::filter_directives(Some("sqlx=debug,my_app=trace"), None, Env::Prod);
        // Dependency defaults come first...
        assert!(filter.starts_with("sqlx=warn,"));
        // ...the app's own level next...
        assert!(filter.contains(",info,"));
        // ...and RUST_LOG last, where `tracing` lets it override both.
        assert!(filter.ends_with("sqlx=debug,my_app=trace"));
    }

    #[test]
    fn filter_level_defaults_to_debug_in_dev_and_info_in_prod() {
        assert!(Settings::filter_directives(None, None, Env::Dev).ends_with(",debug"));
        assert!(Settings::filter_directives(None, None, Env::Prod).ends_with(",info"));
        // An explicit LOG_LEVEL wins over the environment default and may
        // itself be a full directive set.
        assert!(Settings::filter_directives(None, Some("warn"), Env::Dev).ends_with(",warn"));
    }

    #[test]
    fn dependency_defaults_quiet_sqlx_query_logging() {
        // sqlx logs the text of every statement at INFO; at the framework's
        // own default level that would bury everything else.
        let filter = Settings::filter_directives(None, None, Env::Prod);
        assert!(filter.contains("sqlx=warn"));
    }

    #[test]
    fn every_generated_filter_parses() {
        for (rust_log, level, env) in [
            (None, None, Env::Dev),
            (None, None, Env::Prod),
            (Some("debug"), None, Env::Prod),
            (
                Some("full_stack_engine::access=off"),
                Some("warn"),
                Env::Prod,
            ),
        ] {
            let directives = Settings::filter_directives(rust_log, level, env);
            assert!(
                EnvFilter::builder().parse(&directives).is_ok(),
                "should parse: {directives}"
            );
        }
    }

    #[test]
    fn source_chain_walks_every_cause_without_repeating_it() {
        #[derive(Debug, thiserror::Error)]
        #[error("root cause")]
        struct Root;

        #[derive(Debug, thiserror::Error)]
        #[error("while loading the widget")]
        struct Middle(#[source] Root);

        // thiserror's `{0}` interpolation already embeds the cause's message.
        #[derive(Debug, thiserror::Error)]
        #[error("wrapped: {0}")]
        struct Interpolated(#[from] Root);

        assert_eq!(source_chain(&Root), "root cause");
        assert_eq!(
            source_chain(&Middle(Root)),
            "while loading the widget: root cause"
        );
        // ...so the chain walk must not append it a second time.
        assert_eq!(source_chain(&Interpolated(Root)), "wrapped: root cause");
    }

    #[test]
    fn error_detail_keeps_the_whole_chain_for_the_log() {
        #[derive(Debug, thiserror::Error)]
        #[error("connection refused to db:5432")]
        struct Db;

        #[derive(Debug, thiserror::Error)]
        #[error("loading the dashboard")]
        struct Handler(#[source] Db);

        let detail = ErrorDetail::from_error(&Handler(Db), StatusCode::INTERNAL_SERVER_ERROR);
        // Both the context and the cause, so the log says what broke and why.
        assert_eq!(
            detail.log_message,
            "loading the dashboard: connection refused to db:5432"
        );
        assert_eq!(detail.status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn error_detail_from_display_handles_actix_errors() {
        // actix's `ResponseError` is `Debug + Display` but not
        // `std::error::Error`, so there is no chain to walk — the message is
        // all there is, and it must still reach the log.
        let err = actix_web::error::ErrorBadRequest("Json deserialize error");
        let detail = ErrorDetail::from_display(&err, StatusCode::BAD_REQUEST);
        assert_eq!(detail.log_message, "Json deserialize error");
        assert_eq!(detail.status, StatusCode::BAD_REQUEST);
    }

    #[cfg(feature = "otel")]
    #[test]
    fn traces_endpoint_appends_the_signal_path_once() {
        assert_eq!(
            traces_endpoint("http://localhost:4318"),
            "http://localhost:4318/v1/traces"
        );
        assert_eq!(
            traces_endpoint("http://localhost:4318/"),
            "http://localhost:4318/v1/traces"
        );
        // An operator who already pointed at the signal path is not
        // second-guessed.
        assert_eq!(
            traces_endpoint("https://otlp.example.com/v1/traces"),
            "https://otlp.example.com/v1/traces"
        );
    }
}
