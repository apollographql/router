//! Components of a federated GraphQL Server.
//!
//! Most of these modules are of varying interest to different audiences.
//!
//! If your interests are confined to developing plugins, then the following modules
//! are likely to be of most interest to you:
//!
//! * [`self`] - this module (apollo_router) contains high level building blocks for a federated GraphQL router
//!
//! * [`graphql`] - graphql specific functionality for requests, responses, errors
//!
//! * [`layers`] - examples of tower layers used to implement plugins
//!
//! * [`plugin`] - various APIs for implementing a plugin
//!
//! * [`services`] - the various services handling a GraphQL requests,
//!   and APIs for plugins to intercept them

#![cfg_attr(feature = "failfast", allow(unreachable_code))]
#![warn(unreachable_pub)]
#![warn(missing_docs)]

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

#[macro_use]
pub mod metrics;

mod ageing_priority_queue;
mod apollo_studio_interop;
pub(crate) mod axum_factory;
mod batching;
mod cache;
mod compute_job;
mod configuration;
mod context;
mod error;
mod executable;
mod files;
pub mod graphql;
mod http_ext;
mod http_server_factory;
mod introspection;
pub mod layers;
pub(crate) mod logging;
mod orbiter;
mod plugins;
pub(crate) mod protocols;
mod query_planner;
mod router;
mod router_factory;
pub mod services;
pub(crate) mod spec;
mod state_machine;
pub mod test_harness;
pub mod tracer;
mod uplink;

#[doc(hidden)]
pub mod otel_compat;
mod registry;

pub use crate::configuration::Configuration;
pub use crate::configuration::ListenAddr;
pub use crate::context::Context;
pub use crate::context::extensions::Extensions;
pub use crate::context::extensions::sync::ExtensionsMutex;
pub use crate::executable::Executable;
pub use crate::executable::main;
pub use crate::plugins::subscription::notification::Notify;
pub use crate::router::ApolloRouterError;
pub use crate::router::ConfigurationSource;
pub use crate::router::LicenseSource;
pub use crate::router::RouterHttpServer;
pub use crate::router::SchemaSource;
pub use crate::router::ShutdownSource;
pub use crate::router_factory::Endpoint;
pub use crate::test_harness::MockedSubgraphs;
pub use crate::test_harness::TestHarness;
#[cfg(any(test, feature = "snapshot"))]
pub use crate::test_harness::http_snapshot::SnapshotServer;
#[cfg(any(test, feature = "snapshot"))]
pub use crate::test_harness::http_snapshot::standalone::main as snapshot_server;
pub use crate::test_harness::make_fake_batch;
pub use crate::uplink::UplinkConfig;
pub use crate::uplink::license_enforcement::AllowedFeature;

/// Not part of the public API
#[doc(hidden)]
pub mod _private {
    // Reexports for macros
    pub use linkme;
    pub use once_cell;
    pub use serde_json;

    pub use crate::plugin::PLUGINS;
    pub use crate::plugin::PluginFactory;
    // For tests
    pub use crate::plugins::mock_subgraphs::testing_subgraph_call as mock_subgraphs_subgraph_call;
    pub use crate::router_factory::create_test_service_factory_from_yaml;
    pub use crate::services::APOLLO_GRAPH_REF;
    pub use crate::services::APOLLO_KEY;

    pub fn compute_job_queued_count() -> &'static std::sync::atomic::AtomicUsize {
        &crate::compute_job::queue().queued_count
    }
    pub mod telemetry {
        pub use crate::plugins::telemetry::config::AttributeValue;
        pub use crate::plugins::telemetry::resource::ConfigResource;
    }
}
