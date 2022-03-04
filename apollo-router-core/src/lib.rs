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

mod cache;
mod context;
mod error;
mod json_ext;
mod layer;
mod layers;
mod naive_introspection;
mod plugin;
pub mod plugin_utils;
mod query_cache;
mod query_planner;
mod request;
mod response;
mod service_registry;
mod services;
mod spec;
mod traits;

pub use cache::*;
pub use context::*;
pub use error::*;
pub use json_ext::*;
pub use layer::*;
pub use layers::*;
pub use naive_introspection::*;
pub use plugin::*;
pub use query_cache::*;
pub use query_planner::*;
pub use request::*;
pub use response::*;
pub use service_registry::*;
pub use services::*;
pub use spec::*;
pub use traits::*;

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

pub mod reexports {
    pub use serde_json;
    pub use startup;
}
