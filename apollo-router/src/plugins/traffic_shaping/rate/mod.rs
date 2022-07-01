//! Limit the rate at which requests are processed.

mod layer;
#[allow(clippy::module_inception)]
mod rate;
mod service;

pub(crate) use self::layer::RateLimitLayer;
pub(crate) use self::rate::Rate;
pub(crate) use self::service::RateLimit;
