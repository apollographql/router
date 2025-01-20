//! Limit the rate at which requests are processed.

mod error;
pub(crate) mod future;
mod layer;

pub(crate) mod service;

#[allow(clippy::module_inception)]
mod tps;

pub(crate) use self::error::TpsLimited;
pub(crate) use self::layer::TpsLimitLayer;
