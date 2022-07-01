mod field_type;
mod fragments;
mod query;
mod schema;
mod selection;

use displaydoc::Display;
pub(crate) use field_type::*;
pub(crate) use fragments::*;
pub(crate) use query::*;
pub use schema::Schema;
pub(crate) use selection::*;
use thiserror::Error;

/// GraphQL parsing errors.
#[derive(Error, Debug, Display, Clone)]
pub enum SpecError {
    /// selection processing recursion limit exceeded
    RecursionLimitExceeded,
    /// invalid type error, expected another type than '{0}'
    InvalidType(String),
    /// parsing error: {0}
    ParsingError(String),
    /// subscription operation is not supported
    SubscriptionNotSupported,
}
