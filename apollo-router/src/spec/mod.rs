mod field_type;
mod fragments;
mod query;
mod schema;
mod selection;

use displaydoc::Display;
use thiserror::Error;

pub(crate) use field_type::*;
pub(crate) use fragments::*;
pub use query::*;
pub use schema::*;
pub(crate) use selection::*;

#[derive(Error, Debug, Display, Clone)]
pub enum SpecError {
    /// selection processing recursion limit exceeded
    RecursionLimitExceeded,
    /// invalid type error, expected another type than '{0}'
    InvalidType(FieldType),
    /// parsing error: {0}
    ParsingError(String),
    /// subscription operation is not supported
    SubscriptionNotSupported,
}
