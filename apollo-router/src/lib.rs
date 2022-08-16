//! Components of a federated GraphQL Server.
//!
//! Most of these modules are of varying interest to different audiences.
//!
//! If your interests are confined to developing plugins, then the following modules
//! are likely to be of most interest to you:
//!
//! * [`self`] - this module (apollo_router) contains high level building blocks for a federated GraphQL router
//!
//! * [`error`] - the various errors that the router is expected to handle
//!
//! * [`graphql`] - graphql specific functionality for requests, responses, errors
//!
//! * [`layers`] - examples of tower layers used to implement plugins
//!
//! * [`plugin`] - various APIs for implementing a plugin
//!
//! * [`stages`] - the various stages of handling a GraphQL requests,
//!   and APIs for plugins to intercept them
//!
//! Ultimately, you might want to be interested in all aspects of the implementation, in which case
//! you'll want to become familiar with all of the code.

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
mod json_ext;
#[macro_use]
pub mod plugin;

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
mod plugins;
pub mod query_planner;
mod request;
mod response;
mod router;
mod router_factory;
mod services;
mod spec;
pub mod stages;
mod state_machine;
mod test_harness;

pub use crate::configuration::Configuration;
pub use crate::configuration::ListenAddr;
pub use crate::context::Context;
pub use crate::executable::main;
pub use crate::executable::Executable;
pub use crate::router::ConfigurationSource;
pub use crate::router::RouterHttpServer;
pub use crate::router::SchemaSource;
pub use crate::router::ShutdownSource;
pub use crate::services::http_ext;
pub use crate::test_harness::TestHarness;

/// Not part of the public API
#[doc(hidden)]
pub mod _private {
    // Reexports for macros
    pub use router_bridge;
    pub use serde_json;
    pub use startup;

    // For tests
    pub use crate::plugins::telemetry::Telemetry as TelemetryPlugin;
    pub use crate::router_factory::create_test_service_factory_from_yaml;
}

// TODO: clean these up and import from relevant modules instead
pub(crate) use services::*;
pub(crate) use spec::*;
