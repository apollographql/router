//! Starts a server that will handle http graphql requests.

#![cfg_attr(feature = "failfast", allow(unreachable_code))]
#![warn(unreachable_pub)]

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
mod configuration;
mod context;
pub mod error;
mod executable;
mod files;
pub mod graphql;
mod http_server_factory;
mod introspection;
pub mod layers;
pub mod plugin;
pub mod plugins;
pub mod query_planner;
mod reload;
mod request;
mod response;
mod router;
mod router_factory;
pub mod services;
mod spec;
mod state_machine;
pub mod subscriber;
mod traits;

pub use configuration::Configuration;
pub use context::Context;
pub use executable::main;
pub use executable::Executable;
pub use router::ApolloRouter;
pub use router::ConfigurationKind;
pub use router::SchemaKind;
pub use router::ShutdownKind;
pub use router_factory::__create_test_service_factory_from_yaml;
pub use services::http_ext;
pub use spec::Schema;

#[deprecated(note = "use apollo_router::graphql::Request instead")]
pub type Request = graphql::Request;
#[deprecated(note = "use apollo_router::graphql::Response instead")]
pub type Response = graphql::Response;
#[deprecated(note = "use apollo_router::graphql::Error instead")]
pub type Error = graphql::Error;

// TODO: clean these up and import from relevant modules instead
pub(crate) use services::*;
pub(crate) use spec::*;

/// Reexports for macros
#[doc(hidden)]
pub mod _private {
    pub use router_bridge;
    pub use serde_json;
    pub use startup;
}
