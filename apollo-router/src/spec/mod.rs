#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::panic))]

mod field_type;
mod fragments;
pub(crate) mod operation_limits;
pub(crate) mod query;
mod schema;
mod selection;

use displaydoc::Display;
pub(crate) use field_type::*;
pub(crate) use fragments::*;
pub(crate) use query::Query;
pub(crate) use query::TYPENAME;
pub(crate) use schema::Schema;
pub(crate) use selection::*;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::error::ValidationErrors;
use crate::graphql::ErrorExtension;
use crate::graphql::IntoGraphQLErrors;
use crate::json_ext::Object;

pub(crate) const LINK_DIRECTIVE_NAME: &str = "link";
pub(crate) const LINK_URL_ARGUMENT: &str = "url";
pub(crate) const LINK_AS_ARGUMENT: &str = "as";

/// GraphQL parsing errors.
#[derive(Error, Debug, Display, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub(crate) enum SpecError {
    /// missing input file for query
    UnknownFileId,
    /// selection processing recursion limit exceeded
    RecursionLimitExceeded,
    /// invalid type error, expected another type than '{0}'
    InvalidType(String),
    /// cannot query field '{0}' on type '{1}'
    InvalidField(String, String),
    /// parsing error: {0}
    ParsingError(String),
    /// validation error: {0}
    ValidationError(ValidationErrors),
    /// Unknown operation named "{0}"
    UnknownOperation(String),
    /// subscription operation is not supported
    SubscriptionNotSupported,
    /// query hashing failed: {0}
    QueryHashing(String),
}

pub(crate) const GRAPHQL_VALIDATION_FAILURE_ERROR_KEY: &str = "## GraphQLValidationFailure\n";

impl SpecError {
    pub(crate) const fn get_error_key(&self) -> &'static str {
        match self {
            SpecError::ParsingError(_) => "## GraphQLParseFailure\n",
            SpecError::UnknownOperation(_) => "## GraphQLUnknownOperationName\n",
            _ => GRAPHQL_VALIDATION_FAILURE_ERROR_KEY,
        }
    }
}

impl ErrorExtension for SpecError {
    fn extension_code(&self) -> String {
        match self {
            // This code doesn't really make sense, but it's what was used in the past, and it will
            // be obsolete soon with apollo-compiler v1.0. So keep using it instead of introducing
            // a new code that will only exist for a few weeks.
            SpecError::UnknownFileId => "GRAPHQL_VALIDATION_FAILED",
            SpecError::RecursionLimitExceeded => "RECURSION_LIMIT_EXCEEDED",
            SpecError::InvalidType(_) => "INVALID_TYPE",
            SpecError::InvalidField(_, _) => "INVALID_FIELD",
            SpecError::ParsingError(_) => "PARSING_ERROR",
            SpecError::ValidationError(_) => "GRAPHQL_VALIDATION_FAILED",
            SpecError::UnknownOperation(_) => "GRAPHQL_VALIDATION_FAILED",
            SpecError::SubscriptionNotSupported => "SUBSCRIPTION_NOT_SUPPORTED",
            SpecError::QueryHashing(_) => "QUERY_HASHING",
        }
        .to_string()
    }

    fn custom_extension_details(&self) -> Option<Object> {
        let mut obj = Object::new();
        match self {
            SpecError::InvalidType(ty) => {
                obj.insert("type", ty.clone().into());
            }
            SpecError::InvalidField(field, ty) => {
                obj.insert("type", ty.clone().into());
                obj.insert("field", field.clone().into());
            }
            _ => (),
        }

        (!obj.is_empty()).then_some(obj)
    }
}

impl IntoGraphQLErrors for SpecError {
    fn into_graphql_errors(self) -> Result<Vec<crate::graphql::Error>, Self> {
        match self {
            SpecError::ValidationError(e) => {
                e.into_graphql_errors().map_err(SpecError::ValidationError)
            }
            _ => {
                let gql_err = match self.custom_extension_details() {
                    Some(extension_details) => crate::graphql::Error::builder()
                        .message(self.to_string())
                        .extension_code(self.extension_code())
                        .extensions(extension_details)
                        .build(),
                    None => crate::graphql::Error::builder()
                        .message(self.to_string())
                        .extension_code(self.extension_code())
                        .build(),
                };

                Ok(vec![gql_err])
            }
        }
    }
}
