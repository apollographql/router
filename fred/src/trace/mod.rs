#![doc = include_str!("./README.md")]

pub mod disabled;
#[cfg(any(feature = "full-tracing", feature = "partial-tracing"))]
mod enabled;

#[cfg(not(any(feature = "full-tracing", feature = "partial-tracing")))]
pub use disabled::*;
#[cfg(any(feature = "full-tracing", feature = "partial-tracing"))]
pub use enabled::*;
