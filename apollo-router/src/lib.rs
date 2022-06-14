//! Starts a server that will handle http graphql requests.

#![cfg_attr(feature = "failfast", allow(unreachable_code))]

macro_rules! failfast_debug {
    ($($tokens:tt)+) => {{
        tracing::debug!($($tokens)+);
        #[cfg(feature = "failfast")]
        panic!(
            "failfast triggered. \
            Please remove the feature failfast if you don't want to see these panics"
        );
    }};
}

macro_rules! failfast_error {
    ($($tokens:tt)+) => {{
        tracing::error!($($tokens)+);
        #[cfg(feature = "failfast")]
        panic!(
            "failfast triggered. \
            Please remove the feature failfast if you don't want to see these panics"
        );
    }};
}

#[macro_use]
pub mod json_ext;

mod axum_http_server_factory;
mod cache;
pub mod configuration;
mod context;
pub mod error;
mod executable;
mod files;
mod http_server_factory;
mod introspection;
pub mod layers;
#[macro_use]
pub mod plugin;
pub mod plugins;
mod query_cache;
mod query_planner;
mod reload;
mod request;
mod response;
mod router;
mod router_factory;
mod service_registry;
mod services;
mod spec;
mod state_machine;
pub mod subscriber;
mod traits;

pub use context::Context;
pub use executable::{main, Executable};
pub use request::Request;
pub use response::Response;
pub use router::*;
pub use service_registry::*;
pub use services::*;
pub use spec::*;
pub use traits::*;

/// Useful traits.
pub mod prelude {
    pub use crate::traits::*;
}

/// Useful reexports.
pub mod reexports {
    pub use router_bridge;
    pub use serde_json;
    pub use startup;
}
