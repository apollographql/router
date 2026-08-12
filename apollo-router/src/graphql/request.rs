use apollo_json::JsonKind;
use apollo_json::NewValue;
use bytes::Bytes;
use derivative::Derivative;
use serde::Deserialize;
use serde::Serialize;
use serde::de::Error;

use crate::configuration::BatchingMode;
use crate::graphql::json_object::ObjectAccumulator;
use crate::graphql::json_object::empty_object;
use crate::graphql::json_object::is_empty_object;
use crate::json_ext;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;

/// A GraphQL `Request` used to represent both supergraph and subgraph requests.
#[derive(Clone, Derivative, Serialize, Deserialize)]
// Note: `deserialize_from_bytes` ignores unknown members; if adding
// #[serde(deny_unknown_fields)], make it reject them too.
#[serde(rename_all = "camelCase")]
#[derivative(Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Request {
    /// The GraphQL operation (e.g., query, mutation) string.
    ///
    /// For historical purposes, the term "query" is commonly used to refer to
    /// *any* GraphQL operation which might be, e.g., a `mutation`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub query: Option<String>,

    /// The (optional) GraphQL operation name.
    ///
    /// When specified, this name must match the name of an operation in the
    /// GraphQL document.  When excluded, there must exist only a single
    /// operation in the GraphQL document.  Typically, this value is provided as
    /// the `operationName` on an HTTP-sourced GraphQL request.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub operation_name: Option<String>,

    /// The (optional) GraphQL variables in the form of a JSON object.
    ///
    /// When specified, these variables can be referred to in the `query` by
    /// using `$variableName` syntax, where `{"variableName": "value"}` has been
    /// specified as this `variables` value.
    #[serde(
        skip_serializing_if = "is_empty_object",
        default = "empty_object",
        deserialize_with = "deserialize_object_or_null"
    )]
    pub variables: Value,

    /// The (optional) GraphQL `extensions` of a GraphQL request.
    ///
    /// The implementations of extensions are server specific and not specified by
    /// the GraphQL specification.
    /// For example, Apollo projects support [Automated Persisted Queries][APQ]
    /// which are specified in the `extensions` of a request by populating the
    /// `persistedQuery` key within the `extensions` object:
    ///
    /// ```json
    /// {
    ///   "query": "...",
    ///   "variables": { /* ... */ },
    ///   "extensions": {
    ///     "persistedQuery": {
    ///       "version": 1,
    ///       "sha256Hash": "sha256HashOfQuery"
    /// .   }
    ///   }
    /// }
    /// ```
    ///
    /// [APQ]: https://www.apollographql.com/docs/apollo-server/performance/apq/
    /// Note we allow null when deserializing as per [graphql-over-http spec](https://graphql.github.io/graphql-over-http/draft/#sel-EALFPCCBCEtC37P)
    #[serde(
        skip_serializing_if = "is_empty_object",
        default = "empty_object",
        deserialize_with = "deserialize_object_or_null"
    )]
    pub extensions: Value,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            query: None,
            operation_name: None,
            variables: empty_object(),
            extensions: empty_object(),
        }
    }
}

/// Reads a JSON object, mapping an explicit `null` to an empty object.
fn deserialize_object_or_null<'de, D>(deserializer: D) -> Result<Value, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <Option<Value>>::deserialize(deserializer)?;
    Ok(value
        .filter(|value| !value.is_null())
        .unwrap_or_else(empty_object))
}

/// The JSON type of `value`, for error messages shaped like serde's.
fn json_type_name(value: &Value) -> &'static str {
    match value.kind() {
        JsonKind::Null => "null",
        JsonKind::Bool => "a boolean",
        JsonKind::Number => "a number",
        JsonKind::String => "a string",
        JsonKind::Array => "a sequence",
        JsonKind::Object => "a map",
    }
}

/// Reads a string member of a request envelope: absent and `null` are `None`,
/// any other non-string shape is an error.
fn envelope_string(envelope: &Value, key: &str) -> Result<Option<String>, serde_json::Error> {
    match envelope.get(key) {
        None => Ok(None),
        Some(member) => match member.kind() {
            JsonKind::Null => Ok(None),
            JsonKind::String => Ok(member.as_str_owned()),
            _ => Err(serde_json::Error::custom(format!(
                "invalid type: {}, expected a string for `{key}`",
                json_type_name(&member),
            ))),
        },
    }
}

/// Reads an object member of a request envelope: absent and `null` read as an
/// empty object (the [graphql-over-http spec] allows an explicit `null`), any
/// other non-object shape is an error. The object is a subtree of the parsed
/// request, sharing its arena.
///
/// [graphql-over-http spec]: https://graphql.github.io/graphql-over-http/draft/#sel-EALFPCCBCEtC37P
fn envelope_object(envelope: &Value, key: &str) -> Result<Value, serde_json::Error> {
    match envelope.get(key) {
        None => Ok(empty_object()),
        Some(member) => match member.kind() {
            JsonKind::Null => Ok(empty_object()),
            JsonKind::Object => Ok(member),
            _ => Err(serde_json::Error::custom(format!(
                "invalid type: {}, expected a map or null for `{key}`",
                json_type_name(&member),
            ))),
        },
    }
}

/// Builds a GraphQL [`Request`]. Every part of a request is optional; an empty builder
/// yields a request with no query, no operation name, and no variables or extensions.
///
/// ```
/// # use apollo_router::graphql::Request;
/// let request = Request::builder()
///     .query("query Me($id: ID!) { user(id: $id) { name } }")
///     .operation_name("Me")
///     .variable("id", "1")
///     .build();
/// ```
#[derive(Default)]
pub struct RequestBuilder {
    query: Option<String>,
    operation_name: Option<String>,
    variables: ObjectAccumulator,
    extensions: ObjectAccumulator,
}

impl RequestBuilder {
    /// Sets the GraphQL operation string.
    #[must_use]
    pub fn query(self, query: impl Into<String>) -> Self {
        self.and_query(Some(query))
    }

    /// Sets the GraphQL operation string when `query` is `Some`.
    #[must_use]
    pub fn and_query(mut self, query: Option<impl Into<String>>) -> Self {
        self.query = query.map(Into::into);
        self
    }

    /// Sets the name of the operation to run, which must match an operation in the query.
    #[must_use]
    pub fn operation_name(self, operation_name: impl Into<String>) -> Self {
        self.and_operation_name(Some(operation_name))
    }

    /// Sets the name of the operation to run when `operation_name` is `Some`.
    #[must_use]
    pub fn and_operation_name(mut self, operation_name: Option<impl Into<String>>) -> Self {
        self.operation_name = operation_name.map(Into::into);
        self
    }

    /// Adds every member of the JSON object `variables`, replacing variables of the same name.
    #[must_use]
    pub fn variables(mut self, variables: impl Into<Value>) -> Self {
        self.variables.extend(variables.into());
        self
    }

    /// Adds one variable, replacing any variable of the same name.
    #[must_use]
    pub fn variable<'v>(mut self, name: impl Into<String>, value: impl Into<NewValue<'v>>) -> Self {
        self.variables.insert(name, value);
        self
    }

    /// Adds every member of the JSON object `extensions`, replacing extensions under the
    /// same keys.
    #[must_use]
    pub fn extensions(mut self, extensions: impl Into<Value>) -> Self {
        self.extensions.extend(extensions.into());
        self
    }

    /// Adds one extension, replacing any extension under the same key.
    #[must_use]
    pub fn extension<'v>(mut self, key: impl Into<String>, value: impl Into<NewValue<'v>>) -> Self {
        self.extensions.insert(key, value);
        self
    }

    /// Finishes the builder and returns the [`Request`].
    pub fn build(self) -> Request {
        Request {
            query: self.query,
            operation_name: self.operation_name,
            variables: self.variables.build(),
            extensions: self.extensions.build(),
        }
    }
}

impl Request {
    /// Returns a builder that builds a GraphQL [`Request`] from its parts.
    ///
    /// The optionality of the builder's setters matches what a valid GraphQL request
    /// requires at runtime. Tests that need a request without caring about its contents
    /// can use [`Request::fake_builder`] instead.
    pub fn builder() -> RequestBuilder {
        RequestBuilder::default()
    }

    /// Returns a builder that builds a **fake** GraphQL [`Request`] for testing, where a
    /// fully populated request is awkward to assemble. Use [`Request::builder`] for a real
    /// request.
    ///
    /// The setters relax requirements that a real request would impose. Today they match
    /// [`Request::builder`]; that may change in future versions.
    pub fn fake_builder() -> RequestBuilder {
        RequestBuilder::default()
    }

    /// Deserialize as JSON from `&Bytes`.
    ///
    /// The bytes parse once into an apollo-json document — sharing the buffer,
    /// not copying it — and the request reads the envelope by key, so
    /// `variables` and `extensions` are subtrees of that document rather than
    /// values rebuilt member by member.
    pub fn deserialize_from_bytes(data: &Bytes) -> Result<Self, serde_json::Error> {
        let document =
            apollo_json::Document::parse(data.clone()).map_err(serde_json::Error::custom)?;
        Self::from_request_envelope(&document.root_handle())
    }

    /// Reads a GraphQL request from a parsed JSON envelope. Unknown members
    /// are ignored; a wrong-typed member is an error, matching what serde
    /// reported for the same shapes.
    ///
    /// Use this over re-serializing a [`Value`] for [`Request::deserialize_from_bytes`]:
    /// `variables` and `extensions` stay subtrees of the envelope's document.
    pub(crate) fn from_request_envelope(envelope: &Value) -> Result<Self, serde_json::Error> {
        if !envelope.is_object() {
            return Err(serde_json::Error::custom(format!(
                "invalid type: {}, expected a GraphQL request",
                json_type_name(envelope),
            )));
        }
        Ok(Request {
            query: envelope_string(envelope, "query")?,
            operation_name: envelope_string(envelope, "operationName")?,
            variables: envelope_object(envelope, "variables")?,
            extensions: envelope_object(envelope, "extensions")?,
        })
    }

    /// Convert Bytes into a GraphQL [`Request`].
    ///
    /// An error will be produced in the event that the bytes array cannot be
    /// turned into a valid GraphQL `Request`.
    pub(crate) fn batch_from_bytes(bytes: &[u8]) -> Result<Vec<Request>, serde_json::Error> {
        let document =
            apollo_json::Document::parse(bytes.to_vec()).map_err(serde_json::Error::custom)?;

        Request::process_batch_values(document.root_handle())
    }

    fn allocate_result_array(value: &Value) -> Vec<Request> {
        match value.kind() {
            JsonKind::Array => Vec::with_capacity(value.len().unwrap_or(0)),
            _ => Vec::with_capacity(1),
        }
    }

    fn process_batch_values(value: Value) -> Result<Vec<Request>, serde_json::Error> {
        let mut result = Request::allocate_result_array(&value);

        if value.kind() == JsonKind::Array {
            u64_histogram!(
                "apollo.router.operations.batching.size",
                "Number of queries contained within each query batch",
                value.len().unwrap_or(0) as u64,
                mode = BatchingMode::BatchHttpLink.to_string() // Only supported mode right now
            );

            u64_counter!(
                "apollo.router.operations.batching",
                "Total requests with batched operations",
                1,
                mode = BatchingMode::BatchHttpLink.to_string() // Only supported mode right now
            );
            for entry in value.array_iter() {
                result.push(Request::from_request_envelope(&entry)?);
            }
        } else {
            result.push(Request::from_request_envelope(&value)?);
        }
        Ok(result)
    }

    /// PERF(apollo-json): legacy bridge, revisit -- `serde_urlencoded` cannot produce an
    /// apollo-json value, and this reads GET query parameters rather than a response.
    fn process_value(value: &serde_json_bytes::Value) -> Result<Request, serde_json::Error> {
        let operation_name = value.get("operationName").and_then(|name| name.as_str());
        let query = value
            .get("query")
            .and_then(|query| query.as_str())
            .map(String::from);
        let variables = Self::nested_object(value, "variables")?;
        let extensions = Self::nested_object(value, "extensions")?;

        let request = Self::builder()
            .and_query(query)
            .variables(variables)
            .and_operation_name(operation_name)
            .extensions(extensions)
            .build();
        Ok(request)
    }

    /// The object encoded as JSON text in `value[key]`, empty when the key is absent.
    fn nested_object(
        value: &serde_json_bytes::Value,
        key: &str,
    ) -> Result<Value, serde_json::Error> {
        let Some(text) = value.get(key).and_then(|nested| nested.as_str()) else {
            return Ok(empty_object());
        };
        let map: serde_json_bytes::Map<serde_json_bytes::ByteString, serde_json_bytes::Value> =
            serde_json::from_str(text)?;
        Ok(json_ext::from_legacy(&serde_json_bytes::Value::Object(map)))
    }

    /// Convert encoded URL query string parameters (also known as "search
    /// params") into a GraphQL [`Request`].
    ///
    /// An error will be produced in the event that the query string parameters
    /// cannot be turned into a valid GraphQL `Request`.
    pub fn from_urlencoded_query(url_encoded_query: String) -> Result<Request, serde_json::Error> {
        let urldecoded: serde_json_bytes::Value =
            serde_urlencoded::from_bytes(url_encoded_query.as_bytes())
                .map_err(serde_json::Error::custom)?;

        Request::process_value(&urldecoded)
    }
}

