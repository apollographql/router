//! Plugins implementing router customizations.
//!
//! These plugins are compiled into the router and configured via YAML configuration.

pub(crate) mod content_type;
pub(crate) mod csrf;
mod expose_query_plan;
mod forbid_mutations;
mod headers;
pub(crate) mod html_redirect;
mod include_subgraph_errors;
pub(crate) mod override_url;
pub(crate) mod rhai;
pub(crate) mod telemetry;
pub(crate) mod traffic_shaping;
