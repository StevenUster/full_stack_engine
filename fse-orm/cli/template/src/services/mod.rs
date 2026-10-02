//! Hand-written routes — only for what the models can't declare (a
//! multi-step flow, an external API, a custom report). Registered before
//! everything generated, so a route here wins on the same path.

use crate::web;

pub fn configure(cfg: &mut web::ServiceConfig) {
    // cfg.service(my_handler);
    let _ = cfg;
}
