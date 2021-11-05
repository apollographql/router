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

mod error;

pub mod extensions;
mod json_ext;
mod naive_introspection;
mod query_planner;
mod request;
mod response;
mod schema;
mod traits;

pub use error::*;
pub use json_ext::*;
pub use naive_introspection::*;
pub use query_planner::*;
pub use request::*;
pub use response::*;
pub use schema::*;
pub use traits::*;

pub mod prelude {
    pub use crate::json_ext::ValueExt;
    pub use crate::traits::*;
    pub mod graphql {
        pub use crate::*;
    }
    pub use crate::schema::Schema;
}
