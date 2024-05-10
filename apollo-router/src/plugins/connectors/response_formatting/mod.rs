//! This is a drastically simplified version of the GraphQL execution
//! algorithm used to format a Connector JSON response to match the
//! incoming GraphQL operation.
//!
//! The critical functionality is found in [`resolver::resolve_field`]
//! and [`resolver::type_name`].
//!
//! 1. `resolve_field` handles aliases.
//! 2. `type_name` attempts to add `__typename` in the case of abstract types.
//!
//! Currently this code clones the original data to create a new JsonMap.
//! This was necessary because you could select a field with its name *and*
//! one or more aliases, so it can appear in the result multiple times.
//!
//! If we can't determine type names or encounter some other invalid state,
//! we'll return whatever data we have (or null) and emit a diagnostic. The
//! router's existing response formatting code will probably complete the story.
//!
//! [execution]: https://spec.graphql.org/October2021/#sec-Execution
//! [response]: https://spec.graphql.org/October2021/#sec-Response

pub(super) mod engine;
pub(super) mod resolver;
pub(super) mod response;
pub(super) mod result_coercion;

pub(super) use engine::execute;

/// A JSON-compatible dynamically-typed value.
///
/// Note: [`serde_json_bytes::Value`] is similar
/// to [`serde_json::Value`][serde_json_bytes::serde_json::Value]
/// but uses its reference-counted [`ByteString`][serde_json_bytes::ByteString]
/// for string values and map keys.
pub(super) type JsonValue = serde_json_bytes::Value;

/// A JSON-compatible object/map with string keys and dynamically-typed values.
pub(super) type JsonMap = serde_json_bytes::Map<serde_json_bytes::ByteString, JsonValue>;
