#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::panic))]

mod field_type;
mod fragments;
pub(crate) mod query;
mod schema;
mod selection;

use displaydoc::Display;
pub(crate) use field_type::*;
pub(crate) use fragments::*;
use heck::ToShoutySnakeCase;
pub(crate) use query::Query;
pub(crate) use query::TYPENAME;
pub(crate) use schema::Schema;
pub(crate) use selection::*;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::graphql::ErrorExtensionType;

/// GraphQL parsing errors.
#[derive(Error, Debug, Display, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub(crate) enum SpecError {
    /// selection processing recursion limit exceeded
    RecursionLimitExceeded,
    /// invalid type error, expected another type than '{0}'
    InvalidType(String),
    /// cannot query field '{0}' on type '{1}'
    InvalidField(String, String),
    /// parsing error: {0}
    ParsingError(String),
    /// subscription operation is not supported
    SubscriptionNotSupported,
}

impl SpecError {
    pub(crate) const fn get_error_key(&self) -> &'static str {
        match self {
            SpecError::ParsingError(_) => "## GraphQLParseFailure\n",
            _ => "## GraphQLValidationFailure\n",
        }
    }
}

impl ErrorExtensionType for SpecError {
    fn extension_code(&self) -> String {
        match self {
            SpecError::RecursionLimitExceeded => "RecursionLimitExceeded",
            SpecError::InvalidType(_) => "InvalidType",
            SpecError::InvalidField(_, _) => "InvalidField",
            SpecError::ParsingError(_) => "ParsingError",
            SpecError::SubscriptionNotSupported => "SubscriptionNotSupported",
        }
        .to_shouty_snake_case()
    }
}
