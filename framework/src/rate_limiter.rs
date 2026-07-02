use actix_governor::governor::middleware::NoOpMiddleware;
use actix_governor::{Governor, GovernorConfigBuilder, PeerIpKeyExtractor};

/// Rate limiter for authentication endpoints (login, register)
pub fn auth_rate_limiter() -> Governor<PeerIpKeyExtractor, NoOpMiddleware> {
    let config = GovernorConfigBuilder::default()
        .seconds_per_request(10)
        .burst_size(1)
        .finish()
        .expect("Failed to create auth rate limiter config");

    Governor::new(&config)
}

/// Rate limiter for general endpoints
pub fn general_rate_limiter() -> Governor<PeerIpKeyExtractor, NoOpMiddleware> {
    let config = GovernorConfigBuilder::default()
        .seconds_per_request(1)
        .burst_size(100)
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
) -> Governor<PeerIpKeyExtractor, NoOpMiddleware> {
    let config = GovernorConfigBuilder::default()
        .seconds_per_request(seconds_per_request)
        .burst_size(burst_size)
        .finish()
        .expect("Failed to create custom rate limiter config");

    Governor::new(&config)
}
