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
mod http_server_factory;
mod introspection;
pub mod layers;
#[macro_use]
pub mod plugin;
pub mod plugins;
pub mod query_planner;
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

pub use configuration::Configuration;
pub use context::Context;
pub use executable::{main, Executable};
pub use request::Request;
pub use response::Response;
pub use router::{ApolloRouter, ConfigurationKind, SchemaKind, ShutdownKind};
pub use services::http_compat;
pub use services::PluggableRouterServiceBuilder;
pub use services::ResponseBody;
pub use services::{ExecutionRequest, ExecutionResponse, ExecutionService};
pub use services::{QueryPlannerRequest, QueryPlannerResponse};
pub use services::{RouterRequest, RouterResponse, RouterService};
pub use services::{SubgraphRequest, SubgraphResponse, SubgraphService};
pub use spec::Schema;
pub(crate) use spec::*;

/// Reexports for macros
#[doc(hidden)]
pub mod _private {
    pub use router_bridge;
    pub use serde_json;
    pub use startup;
}
