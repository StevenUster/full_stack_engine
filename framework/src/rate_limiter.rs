use actix_governor::governor::middleware::NoOpMiddleware;
use actix_governor::{Governor, GovernorConfigBuilder, PeerIpKeyExtractor};

/// Rate limiter for authentication endpoints (login, register)
/// More restrictive: 5 requests per minute per IP
pub fn auth_rate_limiter() -> Governor<PeerIpKeyExtractor, NoOpMiddleware> {
    let config = GovernorConfigBuilder::default()
        .seconds_per_request(120)
        .burst_size(5)
        .finish()
        .expect("Failed to create auth rate limiter config");

    Governor::new(&config)
}

/// Rate limiter for general endpoints
/// Less restrictive: 100 requests per minute per IP
pub fn _general_rate_limiter() -> Governor<PeerIpKeyExtractor, NoOpMiddleware> {
    let config = GovernorConfigBuilder::default()
        .seconds_per_request(1)
        .burst_size(100)
        .finish()
        .expect("Failed to create general rate limiter config");

    Governor::new(&config)
}
