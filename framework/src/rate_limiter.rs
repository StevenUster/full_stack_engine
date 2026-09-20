//! Per-client-IP rate limiting, as Actix middleware over [`governor`].
//!
//! # Why this is hand-written
//!
//! This used to wrap `actix-governor`. That crate is **GPL-3.0-or-later**, while
//! this framework publishes as `MIT OR Apache-2.0` — so linking it made every
//! binary built on the framework a GPLv3 derivative work, contradicting the
//! declared licence. The algorithm lives in [`governor`] itself, which is MIT;
//! only the Actix glue was GPL, and the framework already supplied its own key
//! extraction, which was most of that glue.
//!
//! So this module is the glue, written against `governor`'s public API:
//! extract a key, ask the limiter, and either call the inner service or answer
//! `429`.
//!
//! # Shared buckets
//!
//! Actix builds the middleware stack **once per worker thread**. A limiter
//! created per worker would multiply the effective limit by the worker count, so
//! every limiter here is an `Arc` handed out from a process-wide cache keyed by
//! call site (see [`shared_limiter`]). One IP therefore gets one bucket for the
//! whole process.

use std::collections::HashMap;
use std::future::{Ready, ready};
use std::hash::Hash;
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use actix_web::body::EitherBody;
use actix_web::dev::{Service, ServiceRequest, ServiceResponse, Transform, forward_ready};
use actix_web::http::header::RETRY_AFTER;
use actix_web::{HttpResponse, HttpResponseBuilder};
use governor::clock::{Clock, DefaultClock};
use governor::state::keyed::DefaultKeyedStateStore;
use governor::{Quota, RateLimiter};

/// A keyed limiter. `DefaultKeyedStateStore` is a sharded `DashMap`, so requests
/// from different addresses don't contend on one lock.
type Limiter<K> = RateLimiter<K, DefaultKeyedStateStore<K>, DefaultClock>;

/// How often the limiter prunes keys whose buckets have fully refilled.
///
/// Without this the keyed store grows by one entry per distinct address, for the
/// life of the process — so a scanner rotating through addresses would be a slow
/// memory leak. `governor::retain_recent` only drops keys indistinguishable from
/// a fresh bucket, so pruning can never hand anyone extra allowance.
const PRUNE_INTERVAL: Duration = Duration::from_mins(1);

/// Extracts the client IP for a request behind a trusted reverse proxy: the
/// right-most `X-Forwarded-For` entry (the address the proxy accepted the
/// connection from, so prepended spoofed values are ignored), then `X-Real-IP`,
/// then the socket peer address. IPv6 is bucketed per `/56` prefix.
///
/// Shared with [`crate::observability`], so the `client.address` on a request's
/// span is the same address the rate limiter keyed on — one answer to "who
/// made this call" instead of two that disagree behind a proxy.
pub(crate) fn client_ip(req: &ServiceRequest) -> Result<IpAddr, KeyError> {
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
        .ok_or(KeyError("could not determine the client address"))?;

    // Rate-limit IPv6 clients per /56 prefix rather than per address, since a
    // single customer is often handed a whole prefix.
    if let IpAddr::V6(ipv6) = ip {
        let mut octets = ipv6.octets();
        octets[7..16].fill(0);
        ip = IpAddr::V6(octets.into());
    }

    Ok(ip)
}

/// A request that cannot be keyed, and so cannot be rate limited.
///
/// The request is **rejected**, not waved through: in practice this only happens
/// when there is no peer address at all (a non-TCP transport), and failing open
/// would turn a misconfiguration into an unmetered endpoint.
#[derive(Debug, Clone, Copy)]
pub struct KeyError(pub &'static str);

/// How a request is keyed for rate-limiting purposes.
///
/// Implement this to limit on something other than the client address — an API
/// token, a tenant id. Whatever the key, it must not be derived from a value the
/// client can choose freely, or a caller can mint themselves a fresh bucket per
/// request.
pub trait RequestKey: Clone + 'static {
    /// One bucket per distinct value.
    type Key: Clone + Eq + Hash + Send + Sync + 'static;

    /// The key for this request.
    ///
    /// `Ok(None)` exempts the request from limiting entirely — return it only
    /// for something the server decides (a path prefix, say), never for
    /// anything the client controls.
    ///
    /// # Errors
    ///
    /// Return [`KeyError`] when no key can be determined; the request is
    /// rejected with `500`.
    fn key(&self, req: &ServiceRequest) -> Result<Option<Self::Key>, KeyError>;
}

/// Keys per client IP, for deployments behind a trusted reverse proxy (the
/// framework's default, since apps bind `0.0.0.0` and terminate TLS at a proxy).
/// See [`client_ip`].
///
/// This assumes a proxy that sets/appends the forwarded headers. If the app is
/// exposed directly to the internet, those headers are client-controlled; key on
/// the socket peer address instead.
#[derive(Clone, Copy)]
pub struct ProxyIp;

impl RequestKey for ProxyIp {
    type Key = IpAddr;

    fn key(&self, req: &ServiceRequest) -> Result<Option<Self::Key>, KeyError> {
        client_ip(req).map(Some)
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

/// Keys per client IP, but exempts configured path prefixes — e.g. a public
/// `/api` consumed server-side by an SSR site, which would otherwise be
/// throttled as one very busy client.
///
/// Exemption is decided **only** by the request path against a list fixed at
/// boot, never from anything the client sends, so it cannot be spoofed to escape
/// limiting on other routes.
#[derive(Clone)]
pub struct ProxyIpExceptPaths {
    exempt_prefixes: Arc<[String]>,
}

impl RequestKey for ProxyIpExceptPaths {
    type Key = IpAddr;

    fn key(&self, req: &ServiceRequest) -> Result<Option<Self::Key>, KeyError> {
        let path = req.path();
        if self
            .exempt_prefixes
            .iter()
            .any(|prefix| path_has_prefix(path, prefix))
        {
            return Ok(None);
        }
        client_ip(req).map(Some)
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// The middleware
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// A limiter plus the pruning bookkeeping that keeps its key store bounded.
struct Shared<K: RequestKey> {
    limiter: Limiter<K::Key>,
    /// Seconds since `start` at which the store was last pruned.
    last_prune: AtomicU64,
    start: std::time::Instant,
}

impl<K: RequestKey> Shared<K> {
    fn new(quota: Quota) -> Self {
        Self {
            limiter: RateLimiter::keyed(quota),
            last_prune: AtomicU64::new(0),
            start: std::time::Instant::now(),
        }
    }

    /// Prunes fully-refilled buckets at most once per [`PRUNE_INTERVAL`].
    ///
    /// Called on the request path rather than from a background task: the check
    /// is one atomic load, and a limiter with no traffic needs no pruning.
    fn maybe_prune(&self) {
        let elapsed = self.start.elapsed().as_secs();
        let last = self.last_prune.load(Ordering::Relaxed);
        if elapsed.saturating_sub(last) < PRUNE_INTERVAL.as_secs() {
            return;
        }
        // Whoever wins the swap does the work; everyone else moves on.
        if self
            .last_prune
            .compare_exchange(last, elapsed, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            self.limiter.retain_recent();
        }
    }
}

/// Rate-limiting middleware. Build one with [`auth_rate_limiter`],
/// [`general_rate_limiter`], [`custom_rate_limiter`] or
/// [`global_rate_limiter`], then `.wrap()` it.
pub struct RateLimit<K: RequestKey> {
    shared: Arc<Shared<K>>,
    key: K,
}

impl<K: RequestKey> Clone for RateLimit<K> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
            key: self.key.clone(),
        }
    }
}

impl<S, B, K> Transform<S, ServiceRequest> for RateLimit<K>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = actix_web::Error> + 'static,
    B: 'static,
    K: RequestKey,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = actix_web::Error;
    type Transform = RateLimitService<S, K>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(RateLimitService {
            service,
            shared: Arc::clone(&self.shared),
            key: self.key.clone(),
        }))
    }
}

/// The instantiated middleware. Not named by callers.
pub struct RateLimitService<S, K: RequestKey> {
    service: S,
    shared: Arc<Shared<K>>,
    key: K,
}

impl<S, B, K> Service<ServiceRequest> for RateLimitService<S, K>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = actix_web::Error> + 'static,
    B: 'static,
    K: RequestKey,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = actix_web::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let outcome = match self.key.key(&req) {
            Err(KeyError(reason)) => Err(refuse(
                HttpResponse::InternalServerError(),
                format!("rate limiting: {reason}"),
            )),
            // Exempt: straight through, no bucket touched.
            Ok(None) => Ok(()),
            Ok(Some(key)) => {
                self.shared.maybe_prune();
                match self.shared.limiter.check_key(&key) {
                    Ok(()) => Ok(()),
                    Err(not_until) => {
                        // Round up: a `Retry-After: 0` invites an immediate
                        // retry that is certain to be refused again.
                        let wait = not_until
                            .wait_time_from(DefaultClock::default().now())
                            .as_secs()
                            .saturating_add(1);
                        let mut response = HttpResponse::TooManyRequests();
                        response.insert_header((RETRY_AFTER, wait.to_string()));
                        Err(refuse(response, "Too many requests".to_string()))
                    }
                }
            }
        };

        match outcome {
            Ok(()) => {
                let fut = self.service.call(req);
                Box::pin(async move { Ok(fut.await?.map_into_left_body()) })
            }
            Err(response) => Box::pin(ready(Ok(req.into_response(response.map_into_right_body())))),
        }
    }
}

/// A refusal body. Plain text, because a rejected request must not cost a
/// template render — that is the work being shed.
fn refuse(mut builder: HttpResponseBuilder, message: String) -> HttpResponse {
    builder
        .content_type("text/plain; charset=utf-8")
        .body(message)
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Constructors
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// `burst` requests immediately, then one more every `seconds_per_request`.
///
/// # Panics
///
/// Panics if either argument is zero — a limiter that allows nothing, or refills
/// infinitely fast, is a programming error rather than a configuration to honour.
fn quota_every(seconds_per_request: u64, burst: u32) -> Quota {
    let burst = NonZeroU32::new(burst).expect("rate limiter burst size must be non-zero");
    let period = Duration::from_secs(seconds_per_request);
    Quota::with_period(period)
        .expect("rate limiter period must be non-zero")
        .allow_burst(burst)
}

/// `per_second` requests per second sustained, with `burst` available at once.
///
/// # Panics
///
/// Panics if either argument is zero.
fn quota_per_second(per_second: u64, burst: u32) -> Quota {
    let burst = NonZeroU32::new(burst).expect("rate limiter burst size must be non-zero");
    // Expressed as a refill interval so a non-integer rate stays exact.
    let period = Duration::from_secs(1)
        .checked_div(u32::try_from(per_second).unwrap_or(u32::MAX))
        .expect("rate limiter rate must be non-zero");
    Quota::with_period(period)
        .expect("rate limiter period must be non-zero")
        .allow_burst(burst)
}

/// The process-wide limiter for a call site, created on first use.
///
/// Apps call `auth_rate_limiter()` & co. inside `configure(...)`, which actix
/// runs once **per worker thread**. Without this cache each worker would get its
/// own buckets and the effective per-IP limit would be multiplied by the worker
/// count. Keyed by call site *and* rate, so distinct endpoints keep separate
/// buckets while every worker shares one per endpoint.
fn shared_limiter<K: RequestKey>(
    caller: &'static std::panic::Location<'static>,
    quota: Quota,
) -> Arc<Shared<K>> {
    type Cache = Mutex<HashMap<CacheKey, Arc<dyn std::any::Any + Send + Sync>>>;
    // Call site plus rate, so two endpoints never share allowance even when
    // they ask for the same numbers.
    type CacheKey = (&'static str, u32, u32, u128, u32);
    static LIMITERS: OnceLock<Cache> = OnceLock::new();

    let cache_key = (
        caller.file(),
        caller.line(),
        caller.column(),
        quota.replenish_interval().as_nanos(),
        quota.burst_size().get(),
    );

    let mut cache = LIMITERS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("rate limiter cache mutex poisoned");

    let entry = cache
        .entry(cache_key)
        .or_insert_with(|| {
            Arc::new(Shared::<K>::new(quota)) as Arc<dyn std::any::Any + Send + Sync>
        })
        .clone();
    drop(cache);

    // The downcast cannot fail: the cache key includes the call site, and one
    // call site always constructs the same `K`.
    entry
        .downcast::<Shared<K>>()
        .expect("one call site always uses one key type")
}

/// Site-wide, per-client-IP rate limiter applied to **every** request, as a
/// first line of defence against L7 floods and abusive scrapers from a single
/// source.
///
/// Built once at boot and cloned into each worker, so the limit is per IP for
/// the whole process rather than per worker thread.
///
/// `exempt_prefixes` are path prefixes that skip the limiter entirely (e.g.
/// `["/api"]` for a public API an SSR site hammers from one address). Exemption
/// is path-based and cannot be spoofed. Keep the list tight — an exempt path has
/// no per-IP cap at all.
///
/// Generous by default so normal browsing (a page load bursts many asset
/// requests) is never affected; tune with `GLOBAL_RATE_LIMIT_PER_SECOND` and
/// `GLOBAL_RATE_LIMIT_BURST` (see [`crate::config`]).
///
/// Note: this is a coarse app-level guard. Volumetric or distributed floods
/// still have to be absorbed upstream (CDN, network layer); per-IP limiting only
/// caps what any one address can do.
///
/// # Panics
///
/// Panics if the configured rate or burst is zero, which [`crate::config`]
/// rejects before this is reached.
#[must_use]
pub fn global_rate_limiter(
    exempt_prefixes: &[String],
    limits: crate::config::RateLimitConfig,
) -> RateLimit<ProxyIpExceptPaths> {
    RateLimit {
        shared: Arc::new(Shared::new(quota_per_second(
            limits.per_second,
            limits.burst,
        ))),
        key: ProxyIpExceptPaths {
            exempt_prefixes: Arc::from(exempt_prefixes.to_vec()),
        },
    }
}

/// Rate limiter for authentication endpoints (login, register): one request per
/// 10 seconds per client. Enforced process-wide.
///
/// # Panics
///
/// Never in practice — the rate is a non-zero constant.
#[must_use]
#[track_caller]
pub fn auth_rate_limiter() -> RateLimit<ProxyIp> {
    RateLimit {
        shared: shared_limiter(std::panic::Location::caller(), quota_every(10, 1)),
        key: ProxyIp,
    }
}

/// Rate limiter for general endpoints: a burst of 100, refilling one per second.
/// Enforced process-wide.
///
/// # Panics
///
/// Never in practice — the rate is a non-zero constant.
#[must_use]
#[track_caller]
pub fn general_rate_limiter() -> RateLimit<ProxyIp> {
    RateLimit {
        shared: shared_limiter(std::panic::Location::caller(), quota_every(1, 100)),
        key: ProxyIp,
    }
}

/// Rate limiter with an app-chosen rate, for endpoints that need something other
/// than the auth/general presets — e.g. `custom_rate_limiter(3600, 1)` for one
/// password-reset email per hour per client. Enforced process-wide.
///
/// # Panics
///
/// Panics if `seconds_per_request` or `burst_size` is zero.
#[must_use]
#[track_caller]
pub fn custom_rate_limiter(seconds_per_request: u64, burst_size: u32) -> RateLimit<ProxyIp> {
    RateLimit {
        shared: shared_limiter(
            std::panic::Location::caller(),
            quota_every(seconds_per_request, burst_size),
        ),
        key: ProxyIp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;
    // Aliased: a bare `test` import shadows the built-in `#[test]` attribute.
    use actix_web::{App, HttpResponse, test as actix_test, web};

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
    fn exempt_prefixes_are_matched_by_path_only() {
        let key = ProxyIpExceptPaths {
            exempt_prefixes: Arc::from(vec!["/api".to_string()]),
        };

        let exempt = TestRequest::with_uri("/api/events")
            .peer_addr("192.0.2.9:4711".parse().unwrap())
            .to_srv_request();
        assert_eq!(key.key(&exempt).unwrap(), None, "/api should be exempt");

        // A path that merely shares the prefix string is still keyed per IP.
        let limited = TestRequest::with_uri("/apidocs")
            .peer_addr("192.0.2.9:4711".parse().unwrap())
            .to_srv_request();
        assert!(key.key(&limited).unwrap().is_some());
    }

    /// The behaviour that actually matters: the n+1'th request in the window is
    /// refused, with a `Retry-After` a client can act on.
    #[actix_web::test]
    async fn requests_beyond_the_burst_are_refused_with_retry_after() {
        let app = actix_test::init_service(
            App::new()
                // One request per hour, burst of 2 — the shape of the
                // password-reset limiter.
                .wrap(custom_rate_limiter(3600, 2))
                .route(
                    "/",
                    web::get().to(|| async { HttpResponse::Ok().body("ok") }),
                ),
        )
        .await;

        let call = || {
            actix_test::call_service(
                &app,
                TestRequest::get()
                    .uri("/")
                    .peer_addr("203.0.113.10:1111".parse().unwrap())
                    .to_request(),
            )
        };

        assert_eq!(call().await.status(), StatusCode::OK);
        assert_eq!(call().await.status(), StatusCode::OK);

        let refused = call().await;
        assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry_after = refused
            .headers()
            .get(RETRY_AFTER)
            .expect("a 429 must say when to come back")
            .to_str()
            .unwrap()
            .parse::<u64>()
            .expect("Retry-After should be a whole number of seconds");
        // Rounded up, so a client obeying it does not retry into another refusal.
        assert!(retry_after >= 1, "Retry-After was {retry_after}");
    }

    /// Buckets are per key: one client exhausting its allowance must not affect
    /// anyone else.
    #[actix_web::test]
    async fn one_clients_limit_does_not_affect_another() {
        let app = actix_test::init_service(App::new().wrap(custom_rate_limiter(3600, 1)).route(
            "/",
            web::get().to(|| async { HttpResponse::Ok().body("ok") }),
        ))
        .await;

        let call = |ip: &str| {
            actix_test::call_service(
                &app,
                TestRequest::get()
                    .uri("/")
                    .peer_addr(format!("{ip}:1111").parse().unwrap())
                    .to_request(),
            )
        };

        assert_eq!(call("203.0.113.20").await.status(), StatusCode::OK);
        assert_eq!(
            call("203.0.113.20").await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        // A different address still has its full allowance.
        assert_eq!(call("203.0.113.21").await.status(), StatusCode::OK);
    }

    /// An exempt path has no bucket at all, however often it is called.
    #[actix_web::test]
    async fn exempt_paths_are_never_limited() {
        let app = actix_test::init_service(
            App::new()
                .wrap(global_rate_limiter(
                    &["/api".to_string()],
                    crate::config::RateLimitConfig {
                        per_second: 1,
                        burst: 1,
                    },
                ))
                .route(
                    "/api/events",
                    web::get().to(|| async { HttpResponse::Ok().body("ok") }),
                )
                .route(
                    "/page",
                    web::get().to(|| async { HttpResponse::Ok().body("ok") }),
                ),
        )
        .await;

        let call = |path: &'static str| {
            actix_test::call_service(
                &app,
                TestRequest::get()
                    .uri(path)
                    .peer_addr("203.0.113.30:1111".parse().unwrap())
                    .to_request(),
            )
        };

        for _ in 0..5 {
            assert_eq!(call("/api/events").await.status(), StatusCode::OK);
        }
        // The same address is still limited on a non-exempt path.
        assert_eq!(call("/page").await.status(), StatusCode::OK);
        assert_eq!(call("/page").await.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    /// Each worker thread builds the middleware stack again; the buckets must
    /// still be shared, or the real limit is multiplied by the worker count.
    #[test]
    fn one_call_site_yields_one_shared_limiter() {
        // One call site executed repeatedly — exactly what actix does when it
        // builds the middleware stack once per worker thread.
        let per_worker = || custom_rate_limiter(60, 5);
        let a = per_worker();
        let b = per_worker();
        assert!(
            Arc::ptr_eq(&a.shared, &b.shared),
            "the same call site must reuse one limiter across workers, or the \
             effective limit is multiplied by the worker count"
        );
        // A clone shares it too.
        assert!(Arc::ptr_eq(&a.shared, &a.clone().shared));
    }

    /// Distinct call sites are distinct endpoints and must not share allowance.
    #[test]
    fn different_call_sites_get_separate_limiters() {
        let a = custom_rate_limiter(60, 5);
        let b = custom_rate_limiter(60, 5);
        assert!(!Arc::ptr_eq(&a.shared, &b.shared));
    }

    #[test]
    fn pruning_keeps_the_key_store_bounded() {
        // A 20ms refill so the test doesn't have to sleep for seconds. A bucket
        // becomes prunable once it is indistinguishable from fresh, which takes
        // a little over its full replenish time.
        let quota = Quota::with_period(Duration::from_millis(20))
            .unwrap()
            .allow_burst(NonZeroU32::new(1).unwrap());
        let shared = Shared::<ProxyIp>::new(quota);

        for octet in 0..50u8 {
            let ip: IpAddr = format!("203.0.113.{octet}").parse().unwrap();
            let _ = shared.limiter.check_key(&ip);
        }
        assert_eq!(shared.limiter.len(), 50, "one bucket per address");

        // Without pruning the store grows by one entry per distinct address for
        // the life of the process. `retain_recent` drops only buckets
        // indistinguishable from fresh, so it can never grant extra allowance.
        std::thread::sleep(Duration::from_millis(200));
        shared.limiter.retain_recent();
        assert!(
            shared.limiter.len() < 50,
            "refilled buckets should be pruned, still {}",
            shared.limiter.len()
        );
    }
}
