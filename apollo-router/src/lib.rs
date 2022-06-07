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

mod axum_http_server_factory;
mod cache;
pub mod configuration;
mod context;
mod error;
mod executable;
mod files;
mod http_server_factory;
mod introspection;
mod json_ext;
pub mod layers;
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

pub use cache::*;
pub use context::*;
pub use error::*;
pub use executable::{main, rt_main};
pub use introspection::*;
pub use json_ext::*;
pub use layers::*;
pub use plugin::*;
pub use query_cache::*;
pub use query_planner::*;
pub use request::*;
pub use response::*;
pub use router::*;
pub use service_registry::*;
pub use services::*;
pub use spec::*;
pub use traits::*;

/// Useful traits.
pub mod prelude {
    // NOTE: only traits can be added here! Everything else should be scoped under the module
    //       graphql so the user can use, for example:
    //        -  graphql::Schema to get a GraphQL Schema
    //        -  graphql::Request to get a GraphQL Request
    //        -  graphql::Response to get a GraphQL Response
    //        -  ...
    //
    //      This is because the user might work with HTTP requests alongside GraphQL requests so we
    //      thought it might be handy to have everything under the namespace "graphql" and let
    //      the user imports things explicitly if they prefer to.
    pub use crate::traits::*;
    pub mod graphql {
        pub use crate::*;
    }
}

/// Useful reexports.
pub mod reexports {
    pub use router_bridge;
    pub use serde_json;
    pub use startup;
}
