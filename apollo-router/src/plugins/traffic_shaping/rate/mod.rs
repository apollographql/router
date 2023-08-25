//! Limit the rate at which requests are processed.

mod error;
pub(crate) mod future;
mod layer;
#[allow(clippy::module_inception)]
mod rate;
pub(crate) mod service;

pub(crate) use self::error::RateLimited;
pub(crate) use self::layer::RateLimitLayer;
pub(crate) use self::rate::Rate;
pub(crate) use self::service::RateLimit;
