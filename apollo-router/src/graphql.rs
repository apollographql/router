//! Types related to GraphQL requests, responses, etc.

use std::fmt;
use std::pin::Pin;

use futures::Stream;
use heck::ToShoutySnakeCase;
pub use router_bridge::planner::Location;
use router_bridge::planner::PlanError;
use router_bridge::planner::PlanErrorExtensions;
use router_bridge::planner::PlannerError;
use router_bridge::planner::WorkerError;
use router_bridge::planner::WorkerGraphQLError;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::json;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JsonMap;
use serde_json_bytes::Value;

use crate::error::FetchError;
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

/// A [GraphQL error](https://spec.graphql.org/October2021/#sec-Errors)
/// as may be found in the `errors` field of a GraphQL [`Response`].
///
/// Converted to (or from) JSON with serde.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Error {
    /// The error message.
    pub message: String,

    /// The locations of the error in the GraphQL document of the originating request.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub locations: Vec<Location>,

    /// If this is a field error, the JSON path to that field in [`Response::data`]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<Path>,

    /// The optional GraphQL extensions for this error.
    #[serde(default, skip_serializing_if = "Object::is_empty")]
    pub extensions: Object,
}
// Implement getter and getter_mut to not use pub field directly

#[buildstructor::buildstructor]
impl Error {
    /// Returns a builder that builds a GraphQL [`Error`] from its components.
    ///
    /// Builder methods:
    ///
    /// * `.message(impl Into<`[`String`]`>)`
    ///   Required.
    ///   Sets [`Error::message`].
    ///
    /// * `.locations(impl Into<`[`Vec`]`<`[`Location`]`>>)`
    ///   Optional.
    ///   Sets the entire `Vec` of [`Error::locations`], which defaults to the empty.
    ///
    /// * `.location(impl Into<`[`Location`]`>)`
    ///   Optional, may be called multiple times.
    ///   Adds one item at the end of [`Error::locations`].
    ///
    /// * `.path(impl Into<`[`Path`]`>)`
    ///   Optional.
    ///   Sets [`Error::path`].
    ///
    /// * `.extensions(impl Into<`[`serde_json_bytes::Map`]`<`[`ByteString`]`, `[`Value`]`>>)`
    ///   Optional.
    ///   Sets the entire [`Error::extensions`] map, which defaults to empty.
    ///
    /// * `.extension(impl Into<`[`ByteString`]`>, impl Into<`[`Value`]`>)`
    ///   Optional, may be called multiple times.
    ///   Adds one item to the [`Error::extensions`] map.
    ///
    /// * `.build()`
    ///   Finishes the builder and returns a GraphQL [`Error`].
    #[builder(visibility = "pub")]
    fn new(
        message: String,
        locations: Vec<Location>,
        path: Option<Path>,
        extension_code: Option<String>,
        // Skip the `Object` type alias in order to use buildstructor’s map special-casing
        mut extensions: JsonMap<ByteString, Value>,
    ) -> Self {
        if let Some(code) = extension_code {
            extensions.entry("code").or_insert_with(|| code.into());
        }

        Self {
            message,
            locations,
            path,
            extensions,
        }
    }

    pub fn from_value(service_name: &str, value: Value) -> Result<Error, FetchError> {
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

/// Displays (only) the error message.
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

/// Trait used to convert expected errors into a list of GraphQL errors
pub(crate) trait IntoGraphQLErrors
where
    Self: Sized,
{
    fn into_graphql_errors(self) -> Result<Vec<Error>, Self>;
}

/// Trait used to get extension type from an error
pub trait ErrorExtension
where
    Self: Sized,
{
    fn extension_code(&self) -> String {
        std::any::type_name::<Self>().to_shouty_snake_case()
    }

    fn custom_extension_details(&self) -> Option<Object> {
        None
    }
}

impl ErrorExtension for PlanError {}

impl From<PlanError> for Error {
    fn from(err: PlanError) -> Self {
        let extension_code = err.extension_code();
        let extensions = err
            .extensions
            .map(convert_extensions_to_map)
            .unwrap_or_else(move || {
                let mut object = Object::new();
                object.insert("code", extension_code.into());
                object
            });
        Self {
            message: err.message.unwrap_or_else(|| String::from("plan error")),
            extensions,
            ..Default::default()
        }
    }
}

impl ErrorExtension for PlannerError {
    fn extension_code(&self) -> String {
        match self {
            PlannerError::WorkerGraphQLError(worker_graphql_error) => worker_graphql_error
                .extensions
                .as_ref()
                .map(|ext| ext.code.clone())
                .unwrap_or_else(|| worker_graphql_error.extension_code()),
            PlannerError::WorkerError(worker_error) => worker_error
                .extensions
                .as_ref()
                .map(|ext| ext.code.clone())
                .unwrap_or_else(|| worker_error.extension_code()),
        }
    }
}

impl From<PlannerError> for Error {
    fn from(err: PlannerError) -> Self {
        match err {
            PlannerError::WorkerGraphQLError(err) => err.into(),
            PlannerError::WorkerError(err) => err.into(),
        }
    }
}

impl ErrorExtension for WorkerError {}

impl From<WorkerError> for Error {
    fn from(err: WorkerError) -> Self {
        let extension_code = err.extension_code();
        let mut extensions = err
            .extensions
            .map(convert_extensions_to_map)
            .unwrap_or_default();
        extensions.insert("code", extension_code.into());

        Self {
            message: err.message.unwrap_or_else(|| String::from("worker error")),
            locations: err.locations.into_iter().map(Location::from).collect(),
            extensions,
            ..Default::default()
        }
    }
}

impl ErrorExtension for WorkerGraphQLError {}

impl From<WorkerGraphQLError> for Error {
    fn from(err: WorkerGraphQLError) -> Self {
        let extension_code = err.extension_code();
        let mut extensions = err
            .extensions
            .map(convert_extensions_to_map)
            .unwrap_or_default();
        extensions.insert("code", extension_code.into());
        Self {
            message: err.message,
            locations: err.locations.into_iter().map(Location::from).collect(),
            extensions,
            ..Default::default()
        }
    }
}

fn convert_extensions_to_map(ext: PlanErrorExtensions) -> Object {
    let mut extensions = Object::new();
    extensions.insert("code", ext.code.into());
    if let Some(exception) = ext.exception {
        extensions.insert(
            "exception",
            json!({
                "stacktrace": serde_json_bytes::Value::from(exception.stacktrace)
            }),
        );
    }

    extensions
}
