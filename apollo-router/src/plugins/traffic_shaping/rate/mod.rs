//! Limit the rate at which requests are processed.
//! Almost the same layer/service than in the tower codebase but this one is a global rate limit

mod layer;
#[allow(clippy::module_inception)]
mod rate;
mod service;

pub(crate) use self::layer::RateLimitLayer;
pub(crate) use self::rate::Rate;
pub(crate) use self::service::RateLimit;
