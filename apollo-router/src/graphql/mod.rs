//! Types related to GraphQL requests, responses, etc.

mod request;
mod response;
mod visitor;

use std::fmt;
use std::pin::Pin;
use std::str::FromStr;

use apollo_compiler::response::GraphQLError as CompilerExecutionError;
use apollo_compiler::response::ResponseDataPathSegment;
use futures::Stream;
use heck::ToShoutySnakeCase;
pub use request::Request;
pub use response::IncrementalResponse;
use response::MalformedResponseError;
pub use response::Response;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JsonMap;
use serde_json_bytes::Value;
use uuid::Uuid;
pub(crate) use visitor::ResponseVisitor;

use crate::json_ext::Object;
use crate::json_ext::Path;
pub use crate::json_ext::Path as JsonPath;
pub use crate::json_ext::PathElement as JsonPathElement;
use crate::spec::query::ERROR_CODE_RESPONSE_VALIDATION;

/// An asynchronous [`Stream`] of GraphQL [`Response`]s.
///
/// In some cases such as with `@defer`, a single HTTP response from the Router
/// may contain multiple GraphQL responses that will be sent at different times
/// (as more data becomes available).
///
/// We represent this in Rust as a stream,
/// even if that stream happens to only contain one item.
pub type ResponseStream = Pin<Box<dyn Stream<Item = Response> + Send>>;

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
#[serde(rename_all = "camelCase")]
/// The error location
pub struct Location {
    /// The line number
    pub line: u32,
    /// The column number
    pub column: u32,
}

/// A [GraphQL error](https://spec.graphql.org/October2021/#sec-Errors)
/// as may be found in the `errors` field of a GraphQL [`Response`].
///
/// Converted to (or from) JSON with serde.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct Error {
    /// The error message.
    pub message: String,

    /// The locations of the error in the GraphQL document of the originating request.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<Location>,

    /// If this is a field error, the JSON path to that field in [`Response::data`]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<Path>,

    /// The optional GraphQL extensions for this error.
    #[serde(skip_serializing_if = "Object::is_empty")]
    pub extensions: Object,

    /// A unique identifier for this error
    #[serde(skip_serializing)]
    apollo_id: Uuid,

    /// Set when an upstream site (connectors, demand_control) has already emitted a span
    /// event for this error. Read by the centralized emit so traces carry exactly one event
    /// per error. Never serialized — must not leak into the user-facing response.
    #[serde(skip)]
    span_event_emitted: bool,
}

impl Default for Error {
    fn default() -> Self {
        Self {
            message: String::new(),
            locations: Vec::new(),
            path: None,
            extensions: Object::new(),
            apollo_id: generate_uuid(),
            span_event_emitted: false,
        }
    }
}

// Implement getter and getter_mut to not use pub field directly

#[buildstructor::buildstructor]
impl Error {
    /// Returns a builder that builds a GraphQL [Request Error].
    ///
    /// Request errors do not have paths.
    ///
    /// ## Builder methods
    /// - `.message(impl Into<`[`String`]`>` - Required: a human-readable error description
    /// - `.extension_code(impl Into<`[`String`]`>` - Recommended: a machine-readable error code
    /// - `.location(impl Into<`[`Location`]`>` - Optional: location in GraphQL source text that
    ///   caused the error
    /// - `.extension(impl Into<`[`ByteString`]`>, impl Into<`[`Value`]`>)` - Optional: add a field
    ///   to the extensions object.
    ///
    /// [Request Error]: https://spec.graphql.org/October2021/#sec-Errors.Request-errors
    #[builder(visibility = "pub")]
    fn request_error_new(
        message: String,
        extension_code: Option<String>,
        locations: Vec<Location>,
        extensions: JsonMap<ByteString, Value>,
    ) -> Self {
        Self::unchecked_builder()
            .message(message)
            .locations(locations)
            .and_extension_code(extension_code)
            .extensions(extensions)
            .build()
    }

    /// Returns a builder that builds a GraphQL [Execution Error].
    ///
    /// Execution errors occur during execution of specific fields.
    ///
    /// ## Builder methods
    /// - `.message(impl Into<`[`String`]`>` - Required: a human-readable error description
    /// - `.path(impl Into<`[`Path`]`>` - Required: path to the field that emitted the error
    /// - `.extension_code(impl Into<`[`String`]`>` - Recommended: a machine-readable error code
    /// - `.location(impl Into<`[`Location`]`>` - Optional: location in GraphQL source text that
    ///   caused the error
    /// - `.extension(impl Into<`[`ByteString`]`>, impl Into<`[`Value`]`>)` - Optional: add a field
    ///   to the extensions object.
    ///
    /// [Execution Error]: https://spec.graphql.org/October2021/#sec-Errors.Field-errors
    #[builder(visibility = "pub")]
    fn execution_error_new(
        message: String,
        path: Path,
        extension_code: Option<String>,
        locations: Vec<Location>,
        extensions: JsonMap<ByteString, Value>,
    ) -> Self {
        Self::unchecked_builder()
            .message(message)
            .path(path)
            .and_extension_code(extension_code)
            .locations(locations)
            .extensions(extensions)
            .build()
    }

    /// Dangerously build any kind of GraphQL error.
    ///
    /// **Always** prefer [Error::request_error_builder] or [Error::execution_error_builder] to
    /// show explicitly what kind of error you're building.
    ///
    /// This variant exists purely to support non-spec use cases and to support overriding fields
    /// that are normally automatically populated.
    #[builder(visibility = "pub")]
    fn unchecked_new(
        message: String,
        locations: Vec<Location>,
        path: Option<Path>,
        extension_code: Option<String>,
        // Skip the `Object` type alias in order to use buildstructor's map special-casing
        mut extensions: JsonMap<ByteString, Value>,
        apollo_id: Option<Uuid>,
    ) -> Self {
        if let Some(code) = extension_code {
            extensions
                .entry("code")
                .or_insert(Value::String(ByteString::from(code)));
        }
        Self {
            message,
            locations,
            path,
            extensions,
            apollo_id: apollo_id.unwrap_or_else(Uuid::new_v4),
            span_event_emitted: false,
        }
    }

    /// Returns true if this error is a [Request Error], meaning it doesn't have a path.
    ///
    /// [Request Error]: https://spec.graphql.org/October2021/#sec-Errors.Request-errors
    #[allow(dead_code)] // TODO(@goto-bus-stop): i'll probably use this
    pub(crate) fn is_request_error(&self) -> bool {
        self.path.is_none()
    }

    /// Returns true if this error is an [Execution Error], meaning it resulted from executing a field.
    ///
    /// [Execution Error]: https://spec.graphql.org/October2021/#sec-Errors.Field-errors
    pub(crate) fn is_execution_error(&self) -> bool {
        self.path.is_some()
    }

    pub(crate) fn from_value(value: Value) -> Result<Error, MalformedResponseError> {
        let mut object = ensure_object!(value).map_err(|error| MalformedResponseError {
            reason: format!("invalid error within `errors`: {error}"),
        })?;

        let extensions =
            extract_key_value_from_object!(object, "extensions", Value::Object(o) => o)
                .map_err(|err| MalformedResponseError {
                    reason: format!("invalid `extensions` within error: {err}"),
                })?
                .unwrap_or_default();
        let message = match extract_key_value_from_object!(object, "message", Value::String(s) => s)
        {
            Ok(Some(s)) => Ok(s.as_str().to_string()),
            Ok(None) => Err(MalformedResponseError {
                reason: "missing required `message` property within error".to_owned(),
            }),
            Err(err) => Err(MalformedResponseError {
                reason: format!("invalid `message` within error: {err}"),
            }),
        }?;
        let locations: Vec<Location> = extract_key_value_from_object!(object, "locations")
            .map(skip_invalid_locations)
            .map(serde_json_bytes::from_value)
            .transpose()
            .map_err(|err| MalformedResponseError {
                reason: format!("invalid `locations` within error: {err}"),
            })?
            .unwrap_or_default();
        let path: Option<Path> = extract_key_value_from_object!(object, "path")
            .map(serde_json_bytes::from_value)
            .transpose()
            .map_err(|err| MalformedResponseError {
                reason: format!("invalid `path` within error: {err}"),
            })?;
        let apollo_id: Option<Uuid> = extract_key_value_from_object!(
            object,
            "apolloId",
            Value::String(s) => s
        )
        .map_err(|err| MalformedResponseError {
            reason: format!("invalid `apolloId` within error: {err}"),
        })?
        .map(|s| {
            Uuid::from_str(s.as_str()).map_err(|err| MalformedResponseError {
                reason: format!("invalid `apolloId` within error: {err}"),
            })
        })
        .transpose()?;

        // We'll use an unchecked builder as it could be either a request error or an execution error
        Ok(Self::unchecked_builder()
            .message(message)
            .and_path(path)
            .locations(locations)
            .extensions(extensions)
            .and_apollo_id(apollo_id)
            .build())
    }

    pub(crate) fn from_value_completion_value(value: &Value) -> Option<Error> {
        let value_completion = ensure_object!(value).ok()?;
        let mut extensions = value_completion
            .get("extensions")
            .and_then(|e: &Value| -> Option<Object> {
                serde_json_bytes::from_value(e.clone()).ok()
            })
            .unwrap_or_default();
        extensions.insert("code", ERROR_CODE_RESPONSE_VALIDATION.into());
        extensions.insert("severity", tracing::Level::WARN.as_str().into());

        let message = value_completion
            .get("message")
            .and_then(|m| m.as_str())
            .map(|m| m.to_string())
            .unwrap_or_default();
        let locations = value_completion
            .get("locations")
            .map(|l: &Value| skip_invalid_locations(l.clone()))
            .map(|l: Value| serde_json_bytes::from_value(l).unwrap_or_default())
            .unwrap_or_default();
        let path =
            value_completion
                .get("path")
                .and_then(|p: &serde_json_bytes::Value| -> Option<Path> {
                    serde_json_bytes::from_value(p.clone()).ok()
                });

        // We'll use an unchecked builder as it could be either a request error or an execution error
        Some(
            Self::unchecked_builder()
                .message(message)
                .and_path(path)
                .locations(locations)
                .extensions(extensions)
                .build(),
        )
    }

    /// Extract the error code from [`Error::extensions`] as a String if it is set.
    pub fn extension_code(&self) -> Option<String> {
        self.extensions.get("code").and_then(|c| match c {
            Value::String(s) => Some(s.as_str().to_owned()),
            Value::Number(n) => Some(n.to_string()),
            Value::Null | Value::Array(_) | Value::Object(_) | Value::Bool(_) => None,
        })
    }

    /// Retrieve the internal Apollo unique ID for this error
    pub fn apollo_id(&self) -> Uuid {
        self.apollo_id
    }

    /// Returns a duplicate of the error where [`self.apollo_id`][Self::apollo_id] is now the given ID
    pub fn with_apollo_id(&self, id: Uuid) -> Self {
        let mut new_err = self.clone();
        new_err.apollo_id = id;
        new_err
    }

    pub(crate) fn span_event_emitted(&self) -> bool {
        self.span_event_emitted
    }

    pub(crate) fn set_span_event_emitted(&mut self, value: bool) {
        self.span_event_emitted = value;
    }

    #[cfg(test)]
    /// Returns a duplicate of the error where [`self.apollo_id`] is `Uuid::nil()`. Used for
    /// comparing errors in tests where you cannot control the randomly generated Uuid
    pub fn with_null_id(&self) -> Self {
        self.with_apollo_id(Uuid::nil())
    }
}

/// Generate a random Uuid. For use in generating a default [`Error::apollo_id`] when not supplied
/// during deserialization.
fn generate_uuid() -> Uuid {
    Uuid::new_v4()
}

/// GraphQL spec require that both "line" and "column" are positive numbers.
/// However GraphQL Java and GraphQL Kotlin return `{ "line": -1, "column": -1 }`
/// if they can't determine error location inside query.
/// This function removes such locations from supplied value.
fn skip_invalid_locations(mut value: Value) -> Value {
    if let Some(array) = value.as_array_mut() {
        array.retain(|location| {
            location.get("line") != Some(&Value::from(-1))
                || location.get("column") != Some(&Value::from(-1))
        })
    }
    value
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
pub(crate) trait ErrorExtension
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

impl From<CompilerExecutionError> for Error {
    fn from(error: CompilerExecutionError) -> Self {
        let CompilerExecutionError {
            message,
            locations,
            path,
            extensions,
        } = error;
        let locations = locations
            .into_iter()
            .map(|location| Location {
                line: location.line as u32,
                column: location.column as u32,
            })
            .collect::<Vec<_>>();
        let path = if !path.is_empty() {
            let elements = path
                .into_iter()
                .map(|element| match element {
                    ResponseDataPathSegment::Field(name) => {
                        JsonPathElement::Key(name.as_str().to_owned(), None)
                    }
                    ResponseDataPathSegment::ListIndex(i) => JsonPathElement::Index(i),
                })
                .collect();
            Some(Path(elements))
        } else {
            None
        };
        Self {
            message,
            locations,
            path,
            extensions,
            apollo_id: Uuid::new_v4(),
            span_event_emitted: false,
        }
    }
}

/// Assert that the expected and actual [`Error`] are equal when ignoring their
/// [`Error::apollo_id`].
#[macro_export]
macro_rules! assert_error_eq_ignoring_id {
    ($expected:expr, $actual:expr) => {
        assert_eq!($expected.with_null_id(), $actual.with_null_id());
    };
}

/// Assert that the expected and actual lists of [`Error`] are equal when ignoring their
/// [`Error::apollo_id`].
#[macro_export]
macro_rules! assert_errors_eq_ignoring_id {
    ($expected:expr, $actual:expr) => {{
        let normalize =
            |v: &[graphql::Error]| v.iter().map(|e| e.with_null_id()).collect::<Vec<_>>();

        assert_eq!(normalize(&$expected), normalize(&$actual));
    }};
}

/// Assert that the expected and actual [`Response`] are equal when ignoring the
/// [`Error::apollo_id`] on any [`Error`] in their [`Response::errors`].
#[macro_export]
macro_rules! assert_response_eq_ignoring_error_id {
    ($expected:expr, $actual:expr) => {{
        let normalize =
            |v: &[graphql::Error]| v.iter().map(|e| e.with_null_id()).collect::<Vec<_>>();
        let mut expected_response: graphql::Response = $expected.clone();
        let mut actual_response: graphql::Response = $actual.clone();
        expected_response.errors = normalize(&expected_response.errors);
        actual_response.errors = normalize(&actual_response.errors);

        assert_eq!(expected_response, actual_response);
    }};
}
