use actix_governor::governor::middleware::NoOpMiddleware;
use actix_governor::{
    Governor, GovernorConfig, GovernorConfigBuilder, KeyExtractor, SimpleKeyExtractionError,
};
use actix_web::dev::ServiceRequest;
use std::net::IpAddr;
use std::sync::Arc;

/// Extracts the client IP for a request behind a trusted reverse proxy: the
/// right-most `X-Forwarded-For` entry (the address the proxy accepted the
/// connection from, so prepended spoofed values are ignored), then `X-Real-IP`,
/// then the socket peer address. IPv6 is bucketed per `/56` prefix.
fn client_ip(req: &ServiceRequest) -> Result<IpAddr, SimpleKeyExtractionError<&'static str>> {
    let forwarded = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit(',').next())
        .and_then(|s| s.trim().parse::<IpAddr>().ok());

    let real_ip = req
        .headers()
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<IpAddr>().ok());

    let mut ip = forwarded
        .or(real_ip)
        .or_else(|| req.peer_addr().map(|socket| socket.ip()))
        .ok_or_else(|| {
            SimpleKeyExtractionError::new("Could not determine client IP for rate limiting")
        })?;

    // Rate-limit IPv6 clients per /56 prefix rather than per address, since a
    // single customer is often handed a whole prefix.
    if let IpAddr::V6(ipv6) = ip {
        let mut octets = ipv6.octets();
        octets[7..16].fill(0);
        ip = IpAddr::V6(octets.into());
    }

    Ok(ip)
}

/// Rate-limit key extractor for deployments behind a trusted reverse proxy
/// (the framework's default, since apps bind `0.0.0.0` and terminate TLS at a
/// proxy). Keys per client IP; see [`client_ip`].
///
/// This assumes a proxy that sets/appends the forwarded headers. If the app is
/// exposed directly to the internet, those headers are client-controlled; key
/// on the socket peer address instead.
#[derive(Clone)]
pub struct ProxyIpKeyExtractor;

impl KeyExtractor for ProxyIpKeyExtractor {
    type Key = IpAddr;
    type KeyExtractionError = SimpleKeyExtractionError<&'static str>;

    fn extract(&self, req: &ServiceRequest) -> Result<Self::Key, Self::KeyExtractionError> {
        client_ip(req)
    }
}

/// True when `path` equals `prefix` or is a child path of it (`/api` matches
/// `/api` and `/api/events`, but not `/apidocs`).
fn path_has_prefix(path: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    prefix.is_empty()
        || path == prefix
        || (path.starts_with(prefix) && path.as_bytes().get(prefix.len()) == Some(&b'/'))
}

/// Key for the site-wide limiter. `Exempt` is produced **only** when a request
/// path matches a configured exempt prefix — never derived from any
/// client-controlled value — so it cannot be spoofed to bypass limiting on
/// non-exempt routes.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum GlobalRateLimitKey {
    Ip(IpAddr),
    Exempt,
}

/// Key extractor for the site-wide limiter: exempts configured path prefixes
/// (e.g. a public `/api` consumed by an SSR site from a single server IP) and
/// otherwise keys per client IP.
#[derive(Clone)]
pub struct GlobalRateLimitKeyExtractor {
    exempt_prefixes: Arc<[String]>,
}

impl KeyExtractor for GlobalRateLimitKeyExtractor {
    type Key = GlobalRateLimitKey;
    type KeyExtractionError = SimpleKeyExtractionError<&'static str>;

    fn extract(&self, req: &ServiceRequest) -> Result<Self::Key, Self::KeyExtractionError> {
        let path = req.path();
        if self
            .exempt_prefixes
            .iter()
            .any(|prefix| path_has_prefix(path, prefix))
        {
            return Ok(GlobalRateLimitKey::Exempt);
        }
        Ok(GlobalRateLimitKey::Ip(client_ip(req)?))
    }

    fn whitelisted_keys(&self) -> Vec<Self::Key> {
        vec![GlobalRateLimitKey::Exempt]
    }
}

/// Site-wide, per-client-IP rate limiter applied to **every** request as a
/// first line of defence against L7 floods / abusive scrapers from a single
/// source. Returns the shared [`GovernorConfig`] (not the middleware) so the
/// same token buckets are shared across all worker threads — the effective
/// limit is per IP for the whole process, not per worker.
///
/// `exempt_prefixes` are path prefixes that skip the limiter entirely (e.g.
/// `["/api"]` for a public API an SSR site hammers from one IP). Exemption is
/// path-based and cannot be spoofed. Keep it empty unless a route genuinely
/// needs to be unmetered — an exempt path has no per-IP cap at all.
///
/// Generous by default so normal browsing (a page load bursts many asset
/// requests) is never affected; tune per deployment with the environment
/// variables `GLOBAL_RATE_LIMIT_PER_SECOND` (default 100) and
/// `GLOBAL_RATE_LIMIT_BURST` (default 500).
///
/// Note: this is a coarse app-level guard. Volumetric / distributed DDoS still
/// needs to be absorbed upstream (CDN / network layer); per-IP limiting only
/// caps what any one address can do.
///
/// # Panics
///
/// Panics if the derived rate or burst is zero (only possible via an explicit
/// `0` override, which is rejected in favour of the default).
#[must_use]
pub fn global_rate_limiter(
    exempt_prefixes: &[String],
) -> GovernorConfig<GlobalRateLimitKeyExtractor, NoOpMiddleware> {
    let per_second = std::env::var("GLOBAL_RATE_LIMIT_PER_SECOND")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(100);
    let burst = std::env::var("GLOBAL_RATE_LIMIT_BURST")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(500);

    GovernorConfigBuilder::default()
        .requests_per_second(per_second)
        .burst_size(burst)
        .key_extractor(GlobalRateLimitKeyExtractor {
            exempt_prefixes: Arc::from(exempt_prefixes.to_vec()),
        })
        .finish()
        .expect("Failed to create global rate limiter config")
}

/// Rate limiter for authentication endpoints (login, register)
pub fn auth_rate_limiter() -> Governor<ProxyIpKeyExtractor, NoOpMiddleware> {
    let config = GovernorConfigBuilder::default()
        .seconds_per_request(10)
        .burst_size(1)
        .key_extractor(ProxyIpKeyExtractor)
        .finish()
        .expect("Failed to create auth rate limiter config");

    Governor::new(&config)
}

/// Rate limiter for general endpoints
pub fn general_rate_limiter() -> Governor<ProxyIpKeyExtractor, NoOpMiddleware> {
    let config = GovernorConfigBuilder::default()
        .seconds_per_request(1)
        .burst_size(100)
        .key_extractor(ProxyIpKeyExtractor)
        .finish()
        .expect("Failed to create general rate limiter config");

    Governor::new(&config)
}

/// Rate limiter with an app-chosen request rate, for endpoints that need
/// something other than the auth/general presets (e.g. one password-reset
/// email per hour per IP).
///
/// # Panics
///
/// Panics if `seconds_per_request` or `burst_size` is zero.
#[must_use]
pub fn custom_rate_limiter(
    seconds_per_request: u64,
    burst_size: u32,
) -> Governor<ProxyIpKeyExtractor, NoOpMiddleware> {
    let config = GovernorConfigBuilder::default()
        .seconds_per_request(seconds_per_request)
        .burst_size(burst_size)
        .key_extractor(ProxyIpKeyExtractor)
        .finish()
        .expect("Failed to create custom rate limiter config");

    Governor::new(&config)
}
