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
/// Note: this is a coarse app-level guard. Volumetric / distributed `DDoS` still
/// needs to be absorbed upstream (CDN / network layer); per-IP limiting only
/// caps what any one address can do.
///
/// # Panics
///
/// Panics if the derived rate or burst is zero (only possible via an explicit
/// `0` override, which is rejected in favour of the default).
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

type ProxyGovernorConfig = GovernorConfig<ProxyIpKeyExtractor, NoOpMiddleware>;

/// Returns the shared limiter config for a call site, creating it on first
/// use. Apps call `auth_rate_limiter()` & co. inside `configure(...)`, which
/// actix runs once **per worker thread** — without this cache each worker
/// would get its own token buckets and the effective per-IP limit would be
/// multiplied by the number of workers. The config holds an `Arc` to the
/// buckets, so every clone for the same call site enforces one shared limit.
/// Keyed by call site (plus rate parameters), so distinct endpoints keep
/// separate buckets.
fn shared_config(
    caller: &'static std::panic::Location<'static>,
    seconds_per_request: u64,
    burst_size: u32,
) -> ProxyGovernorConfig {
    type ConfigKey = (&'static str, u32, u32, u64, u32);
    static CONFIGS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<ConfigKey, ProxyGovernorConfig>>,
    > = std::sync::OnceLock::new();

    CONFIGS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap()
        .entry((
            caller.file(),
            caller.line(),
            caller.column(),
            seconds_per_request,
            burst_size,
        ))
        .or_insert_with(|| {
            GovernorConfigBuilder::default()
                .seconds_per_request(seconds_per_request)
                .burst_size(burst_size)
                .key_extractor(ProxyIpKeyExtractor)
                .finish()
                .expect("Failed to create rate limiter config")
        })
        .clone()
}

/// Rate limiter for authentication endpoints (login, register). The limit is
/// enforced process-wide (shared across worker threads).
///
/// # Panics
///
/// Never in practice: the preset rate and burst are non-zero constants, which
/// is the only config the builder rejects.
#[must_use]
#[track_caller]
pub fn auth_rate_limiter() -> Governor<ProxyIpKeyExtractor, NoOpMiddleware> {
    Governor::new(&shared_config(std::panic::Location::caller(), 10, 1))
}

/// Rate limiter for general endpoints. The limit is enforced process-wide
/// (shared across worker threads).
///
/// # Panics
///
/// Never in practice: the preset rate and burst are non-zero constants, which
/// is the only config the builder rejects.
#[must_use]
#[track_caller]
pub fn general_rate_limiter() -> Governor<ProxyIpKeyExtractor, NoOpMiddleware> {
    Governor::new(&shared_config(std::panic::Location::caller(), 1, 100))
}

/// Rate limiter with an app-chosen request rate, for endpoints that need
/// something other than the auth/general presets (e.g. one password-reset
/// email per hour per IP). The limit is enforced process-wide (shared across
/// worker threads).
///
/// # Panics
///
/// Panics if `seconds_per_request` or `burst_size` is zero.
#[must_use]
#[track_caller]
pub fn custom_rate_limiter(
    seconds_per_request: u64,
    burst_size: u32,
) -> Governor<ProxyIpKeyExtractor, NoOpMiddleware> {
    Governor::new(&shared_config(
        std::panic::Location::caller(),
        seconds_per_request,
        burst_size,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;

    #[test]
    fn path_has_prefix_matches_exact_and_children_only() {
        assert!(path_has_prefix("/api", "/api"));
        assert!(path_has_prefix("/api/events", "/api"));
        assert!(path_has_prefix("/api/events", "/api/"));
        // Sibling paths that merely share the prefix string are not matched.
        assert!(!path_has_prefix("/apidocs", "/api"));
        assert!(!path_has_prefix("/foo", "/api"));
    }

    #[test]
    fn client_ip_ignores_spoofed_forwarded_entries() {
        // A client can prepend arbitrary values to X-Forwarded-For; only the
        // right-most entry (appended by the trusted proxy) is used.
        let req = TestRequest::default()
            .insert_header(("X-Forwarded-For", "6.6.6.6, 203.0.113.7"))
            .to_srv_request();
        assert_eq!(
            client_ip(&req).unwrap(),
            "203.0.113.7".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn client_ip_falls_back_to_real_ip_then_peer() {
        let req = TestRequest::default()
            .insert_header(("X-Real-IP", "198.51.100.4"))
            .to_srv_request();
        assert_eq!(
            client_ip(&req).unwrap(),
            "198.51.100.4".parse::<IpAddr>().unwrap()
        );

        let req = TestRequest::default()
            .peer_addr("192.0.2.9:4711".parse().unwrap())
            .to_srv_request();
        assert_eq!(
            client_ip(&req).unwrap(),
            "192.0.2.9".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn client_ip_buckets_ipv6_per_56_prefix() {
        let ip_for = |addr: &str| {
            let req = TestRequest::default()
                .insert_header(("X-Real-IP", addr))
                .to_srv_request();
            client_ip(&req).unwrap()
        };

        // Same /56: one customer prefix maps to one bucket.
        assert_eq!(
            ip_for("2001:db8:1:100::1"),
            ip_for("2001:db8:1:1ff:aaaa:bbbb:cccc:dddd")
        );
        // Different /56 prefixes stay separate buckets.
        assert_ne!(ip_for("2001:db8:1:100::1"), ip_for("2001:db8:2:100::1"));
    }

    #[test]
    fn global_key_extractor_exempts_configured_prefixes_only() {
        let extractor = GlobalRateLimitKeyExtractor {
            exempt_prefixes: Arc::from(vec!["/api".to_string()]),
        };

        let exempt = TestRequest::with_uri("/api/events")
            .peer_addr("192.0.2.9:4711".parse().unwrap())
            .to_srv_request();
        assert!(matches!(
            extractor.extract(&exempt).unwrap(),
            GlobalRateLimitKey::Exempt
        ));

        // A path that merely shares the prefix string is still keyed per IP.
        let limited = TestRequest::with_uri("/apidocs")
            .peer_addr("192.0.2.9:4711".parse().unwrap())
            .to_srv_request();
        assert!(matches!(
            extractor.extract(&limited).unwrap(),
            GlobalRateLimitKey::Ip(_)
        ));
    }
}
