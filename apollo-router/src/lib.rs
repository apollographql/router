//! Components of a federated GraphQL Server.
//!
//! Most of these modules are of varying interest to different audiences.
//!
//! If your interests are confined to developing plugins, then the following modules
//! are likely to be of most interest to you: [`self`] [`error`] [`graphql`] [`layers`] [`plugin`] [`services`]
//!
//! self - this module (apollo_router) contains high level building blocks for a federated GraphQL router
//!
//! error - the various errors that the router is expected to handle
//!
//! graphql - graphql specific functionality for requests, responses, errors
//!
//! layers - examples of tower layers used to implement plugins
//!
//! plugin - various APIs for implementing a plugin
//!
//! services - definition of the various services a plugin may process
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

pub use configuration::*;
pub use context::Context;
pub use executable::*;
pub use router::*;
#[doc(hidden)]
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
