//! plugins implementing router customizations.
//!
//! These plugins are compiled into the router and configured via YAML configuration.

pub mod csrf;
mod forbid_mutations;
mod headers;
mod include_subgraph_errors;
pub mod override_url;
pub mod rhai;
pub mod telemetry;
pub(crate) mod traffic_shaping;
