//! Namespace for the GraphQL [`Request`], [`Response`], and [`Error`] types.

#![allow(missing_docs)] // FIXME

use std::fmt;
use std::ops::Deref;
use std::pin::Pin;

use futures::Stream;
use router_bridge::planner::PlanErrors;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JsonMap;
use serde_json_bytes::Value;

use crate::error::CacheResolverError;
use crate::error::FetchError;
pub use crate::error::Location;
use crate::error::QueryPlannerError;
use crate::json_ext::Object;
use crate::json_ext::Path;
pub use crate::json_ext::Path as JsonPath;
pub use crate::json_ext::PathElement as JsonPathElement;
pub use crate::request::Request;
pub use crate::response::IncrementalResponse;
pub use crate::response::Response;

/// An asynchronous [`Stream`] of GraphQL [`Response`]s.
///
/// In some cases such as with `@defer`, a single HTTP response from the Router
/// may contain multiple GraphQL responses that will be sent at different times
/// (as more data becomes available).
///
/// We represent this in Rust as a stream,
/// even if that stream happens to only contain one item.
pub type ResponseStream = Pin<Box<dyn Stream<Item = Response> + Send>>;

/// Any GraphQL error.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Error {
    /// The error message.
    pub message: String,

    /// The locations of the error from the originating request.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub locations: Vec<Location>,

    /// The path of the error.
    pub path: Option<Path>,

    /// The optional graphql extensions.
    #[serde(default, skip_serializing_if = "Object::is_empty")]
    pub extensions: Object,
}

#[buildstructor::buildstructor]
impl Error {
    #[builder(visibility = "pub")]
    fn new(
        message: String,
        locations: Vec<Location>,
        path: Option<Path>,
        // Skip the `Object` type alias in order to use buildstructor’s map special-casing
        extensions: JsonMap<ByteString, Value>,
    ) -> Self {
        Self {
            message,
            locations,
            path,
            extensions,
        }
    }

    pub(crate) fn from_value(service_name: &str, value: Value) -> Result<Error, FetchError> {
        let mut object =
            ensure_object!(value).map_err(|error| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: error.to_string(),
            })?;

        let extensions =
            extract_key_value_from_object!(object, "extensions", Value::Object(o) => o)
                .map_err(|err| FetchError::SubrequestMalformedResponse {
                    service: service_name.to_string(),
                    reason: err.to_string(),
                })?
                .unwrap_or_default();
        let message = extract_key_value_from_object!(object, "message", Value::String(s) => s)
            .map_err(|err| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: err.to_string(),
            })?
            .map(|s| s.as_str().to_string())
            .unwrap_or_default();
        let locations = extract_key_value_from_object!(object, "locations")
            .map(serde_json_bytes::from_value)
            .transpose()
            .map_err(|err| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: err.to_string(),
            })?
            .unwrap_or_default();
        let path = extract_key_value_from_object!(object, "path")
            .map(serde_json_bytes::from_value)
            .transpose()
            .map_err(|err| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: err.to_string(),
            })?;

        Ok(Error {
            message,
            locations,
            path,
            extensions,
        })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl From<PlanErrors> for Error {
    fn from(p_err: PlanErrors) -> Self {
        if p_err.errors.len() == 1 {
            let error = p_err.errors[0].clone();
            Self {
                message: error
                    .message
                    .unwrap_or_else(|| String::from("query plan error")),
                extensions: error
                    .extensions
                    .map(|ext| {
                        let mut map = serde_json_bytes::map::Map::new();
                        map.insert("code", Value::from(ext.code));

                        map
                    })
                    .unwrap_or_default(),
                ..Default::default()
            }
        } else {
            Self {
                message: p_err
                    .errors
                    .iter()
                    .filter_map(|err| err.message.clone())
                    .collect::<Vec<String>>()
                    .join("\n"),
                ..Default::default()
            }
        }
    }
}

impl From<QueryPlannerError> for Error {
    fn from(qp_error: QueryPlannerError) -> Self {
        match &qp_error {
            QueryPlannerError::PlanningErrors(plan_errors) => return plan_errors.clone().into(),
            QueryPlannerError::CacheResolverError(cache_err) => {
                // This mess is caused by BoxError
                let CacheResolverError::RetrievalError(retrieval_error) = cache_err.deref();
                if let Some(qp_err) = retrieval_error.deref().downcast_ref::<QueryPlannerError>() {
                    return qp_err.clone().into();
                }
            }
            _ => (),
        }

        Self {
            message: qp_error.to_string(),
            ..Default::default()
        }
    }
}
