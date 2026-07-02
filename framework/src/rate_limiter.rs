use actix_governor::governor::middleware::NoOpMiddleware;
use actix_governor::{
    Governor, GovernorConfig, GovernorConfigBuilder, KeyExtractor, SimpleKeyExtractionError,
};
use actix_web::dev::ServiceRequest;
use std::net::IpAddr;

/// Rate-limit key extractor for deployments behind a trusted reverse proxy
/// (the framework's default, since apps bind `0.0.0.0` and terminate TLS at a
/// proxy).
///
/// It keys on the right-most `X-Forwarded-For` entry — the address the proxy
/// actually accepted the connection from — so a client cannot mint unlimited
/// rate-limit buckets by prepending spoofed `X-Forwarded-For` values. It falls
/// back to `X-Real-IP` and finally the socket peer address.
///
/// This assumes a proxy that sets/appends those headers. If the app is exposed
/// directly to the internet, the forwarded headers are fully client-controlled;
/// key on the socket peer address instead.
#[derive(Clone)]
pub struct ProxyIpKeyExtractor;

impl KeyExtractor for ProxyIpKeyExtractor {
    type Key = IpAddr;
    type KeyExtractionError = SimpleKeyExtractionError<&'static str>;

    fn extract(&self, req: &ServiceRequest) -> Result<Self::Key, Self::KeyExtractionError> {
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
}

/// Site-wide, per-client-IP rate limiter applied to **every** request as a
/// first line of defence against L7 floods / abusive scrapers from a single
/// source. Returns the shared [`GovernorConfig`] (not the middleware) so the
/// same token buckets are shared across all worker threads — the effective
/// limit is per IP for the whole process, not per worker.
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
pub fn global_rate_limiter() -> GovernorConfig<ProxyIpKeyExtractor, NoOpMiddleware> {
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
        .key_extractor(ProxyIpKeyExtractor)
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
