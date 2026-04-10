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
