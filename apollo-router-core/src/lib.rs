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
mod error;
mod json_ext;
mod naive_introspection;
mod query_cache;
mod query_planner;
mod request;
mod response;
mod spec;
mod traits;

pub use cache::*;
pub use error::*;
pub use json_ext::*;
pub use naive_introspection::*;
pub use query_cache::*;
pub use query_planner::*;
pub use request::*;
pub use response::*;
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
