//! plugins implementing router customizations.
//!
//! These plugins are compiled into the router and configured via YAML configuration.

mod forbid_mutations;
mod headers;
mod include_subgraph_errors;
pub mod serde_utils;
mod traffic_shaping;
