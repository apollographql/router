use apollo_json::JsonKind;
use apollo_json::NewValue;
use bytes::Bytes;
use derivative::Derivative;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeSeed;
use serde::de::Error;

use crate::configuration::BatchingMode;
use crate::graphql::json_object::ObjectAccumulator;
use crate::graphql::json_object::empty_object;
use crate::graphql::json_object::is_empty_object;
use crate::json_ext;
use crate::json_ext::Value;

/// A GraphQL `Request` used to represent both supergraph and subgraph requests.
#[derive(Clone, Derivative, Serialize, Deserialize)]
// Note: if adding #[serde(deny_unknown_fields)],
// also remove `Fields::Other` in `DeserializeSeed` impl.
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

/// PERF(apollo-json): legacy bridge, revisit -- `serde_json` drives request parsing,
/// and an apollo-json value only captures a subtree from apollo-json's own
/// deserializers, so the zero-copy `BytesSeed` reads the legacy representation and
/// the object converts here, once per request.
fn as_optional_object<E: Error>(value: serde_json_bytes::Value) -> Result<Value, E> {
    use serde::de::Unexpected;

    let exp = "a map or null";
    match value {
        serde_json_bytes::Value::Object(_) => Ok(json_ext::from_legacy(&value)),
        // Similar to `deserialize_object_or_null`:
        serde_json_bytes::Value::Null => Ok(empty_object()),
        serde_json_bytes::Value::Bool(value) => Err(E::invalid_type(Unexpected::Bool(value), &exp)),
        serde_json_bytes::Value::Number(_) => {
            Err(E::invalid_type(Unexpected::Other("a number"), &exp))
        }
        serde_json_bytes::Value::String(value) => {
            Err(E::invalid_type(Unexpected::Str(value.as_str()), &exp))
        }
        serde_json_bytes::Value::Array(_) => Err(E::invalid_type(Unexpected::Seq, &exp)),
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
    pub fn variable(mut self, name: impl Into<String>, value: impl Into<NewValue>) -> Self {
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
    pub fn extension(mut self, key: impl Into<String>, value: impl Into<NewValue>) -> Self {
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

    /// Deserialize as JSON from `&Bytes`, avoiding string copies where possible
    pub fn deserialize_from_bytes(data: &Bytes) -> Result<Self, serde_json::Error> {
        let seed = RequestFromBytesSeed(data);
        let mut de = serde_json::Deserializer::from_slice(data);
        seed.deserialize(&mut de)
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
                result.push(Request::deserialize_from_bytes(&entry.to_bytes())?);
            }
        } else {
            result.push(Request::deserialize_from_bytes(&value.to_bytes())?);
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

struct RequestFromBytesSeed<'data>(&'data Bytes);

impl<'de> DeserializeSeed<'de> for RequestFromBytesSeed<'_> {
    type Value = Request;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        #[serde(field_identifier, rename_all = "camelCase")]
        enum Field {
            Query,
            OperationName,
            Variables,
            Extensions,
            #[serde(other)]
            Other,
        }

        const FIELDS: &[&str] = &["query", "operationName", "variables", "extensions"];

        struct RequestVisitor<'data>(&'data Bytes);

        impl<'de> serde::de::Visitor<'de> for RequestVisitor<'_> {
            type Value = Request;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a GraphQL request")
            }

            fn visit_map<V>(self, mut map: V) -> Result<Request, V::Error>
            where
                V: serde::de::MapAccess<'de>,
            {
                let mut query = None;
                let mut operation_name = None;
                let mut variables = None;
                let mut extensions = None;
                while let Some(key) = map.next_key()? {
                    match key {
                        Field::Query => {
                            if query.is_some() {
                                return Err(Error::duplicate_field("query"));
                            }
                            query = Some(map.next_value()?);
                        }
                        Field::OperationName => {
                            if operation_name.is_some() {
                                return Err(Error::duplicate_field("operationName"));
                            }
                            operation_name = Some(map.next_value()?);
                        }
                        Field::Variables => {
                            if variables.is_some() {
                                return Err(Error::duplicate_field("variables"));
                            }
                            let seed = serde_json_bytes::value::BytesSeed::new(self.0);
                            let value = map.next_value_seed(seed)?;
                            variables = Some(as_optional_object(value)?);
                        }
                        Field::Extensions => {
                            if extensions.is_some() {
                                return Err(Error::duplicate_field("extensions"));
                            }
                            let seed = serde_json_bytes::value::BytesSeed::new(self.0);
                            let value = map.next_value_seed(seed)?;
                            extensions = Some(as_optional_object(value)?);
                        }
                        Field::Other => {
                            let _: serde::de::IgnoredAny = map.next_value()?;
                        }
                    }
                }
                Ok(Request {
                    query: query.unwrap_or_default(),
                    operation_name: operation_name.unwrap_or_default(),
                    variables: variables.unwrap_or_else(empty_object),
                    extensions: extensions.unwrap_or_else(empty_object),
                })
            }
        }

        deserializer.deserialize_struct("Request", FIELDS, RequestVisitor(self.0))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use test_log::test;

    use super::*;
    use crate::json_ext::json_value;

    #[test]
    fn test_request() {
        let data = json!(
        {
          "query": "query aTest($arg1: String!) { test(who: $arg1) }",
          "operationName": "aTest",
          "variables": { "arg1": "me" },
          "extensions": {"extension": 1}
        });
        let result = check_deserialization(data);
        assert_eq!(
            result,
            Request::builder()
                .query("query aTest($arg1: String!) { test(who: $arg1) }".to_owned())
                .operation_name("aTest")
                .variables(json_value!({ "arg1": "me" }))
                .extensions(json_value!({"extension": 1}))
                .build()
        );
    }

    #[test]
    fn test_no_variables() {
        let result = check_deserialization(json!(
        {
          "query": "query aTest($arg1: String!) { test(who: $arg1) }",
          "operationName": "aTest",
          "extensions": {"extension": 1}
        }));
        assert_eq!(
            result,
            Request::builder()
                .query("query aTest($arg1: String!) { test(who: $arg1) }".to_owned())
                .operation_name("aTest")
                .extensions(json_value!({"extension": 1}))
                .build()
        );
    }

    #[test]
    // rover sends { "variables": null } when running the introspection query,
    // and possibly running other queries as well.
    fn test_variables_is_null() {
        let result = check_deserialization(json!(
        {
          "query": "query aTest($arg1: String!) { test(who: $arg1) }",
          "operationName": "aTest",
          "variables": null,
          "extensions": {"extension": 1}
        }));
        assert_eq!(
            result,
            Request::builder()
                .query("query aTest($arg1: String!) { test(who: $arg1) }")
                .operation_name("aTest")
                .extensions(json_value!({"extension": 1}))
                .build()
        );
    }

    #[test]
    fn from_urlencoded_query_works() {
        let query_string = "query=%7B+topProducts+%7B+upc+name+reviews+%7B+id+product+%7B+name+%7D+author+%7B+id+name+%7D+%7D+%7D+%7D&extensions=%7B+%22persistedQuery%22+%3A+%7B+%22version%22+%3A+1%2C+%22sha256Hash%22+%3A+%2220a101de18d4a9331bfc4ccdfef33cc735876a689490433570f17bdd4c0bad3f%22+%7D+%7D".to_string();

        let expected_result = check_deserialization(json!(
        {
          "query": "{ topProducts { upc name reviews { id product { name } author { id name } } } }",
          "extensions": {
              "persistedQuery": {
                  "version": 1,
                  "sha256Hash": "20a101de18d4a9331bfc4ccdfef33cc735876a689490433570f17bdd4c0bad3f"
              }
            }
        }));

        let req = Request::from_urlencoded_query(query_string).unwrap();

        assert_eq!(expected_result, req);
    }

    #[test]
    fn from_urlencoded_query_with_variables_works() {
        let query_string = "query=%7B+topProducts+%7B+upc+name+reviews+%7B+id+product+%7B+name+%7D+author+%7B+id+name+%7D+%7D+%7D+%7D&variables=%7B%22date%22%3A%222022-01-01T00%3A00%3A00%2B00%3A00%22%7D&extensions=%7B+%22persistedQuery%22+%3A+%7B+%22version%22+%3A+1%2C+%22sha256Hash%22+%3A+%2220a101de18d4a9331bfc4ccdfef33cc735876a689490433570f17bdd4c0bad3f%22+%7D+%7D".to_string();

        let expected_result = check_deserialization(json!(
        {
          "query": "{ topProducts { upc name reviews { id product { name } author { id name } } } }",
          "variables": {"date": "2022-01-01T00:00:00+00:00"},
          "extensions": {
              "persistedQuery": {
                  "version": 1,
                  "sha256Hash": "20a101de18d4a9331bfc4ccdfef33cc735876a689490433570f17bdd4c0bad3f"
              }
            }
        }));

        let req = Request::from_urlencoded_query(query_string).unwrap();

        assert_eq!(expected_result, req);
    }

    #[test]
    fn null_extensions() {
        let expected_result = check_deserialization(json!(
        {
          "query": "{ topProducts { upc name reviews { id product { name } author { id name } } } }",
          "variables": {"date": "2022-01-01T00:00:00+00:00"},
          "extensions": null
        }));
        insta::assert_yaml_snapshot!(expected_result);
    }

    #[test]
    fn missing_extensions() {
        let expected_result = check_deserialization(json!(
        {
          "query": "{ topProducts { upc name reviews { id product { name } author { id name } } } }",
          "variables": {"date": "2022-01-01T00:00:00+00:00"},
        }));
        insta::assert_yaml_snapshot!(expected_result);
    }

    #[test]
    fn extensions() {
        let expected_result = check_deserialization(json!(
        {
          "query": "{ topProducts { upc name reviews { id product { name } author { id name } } } }",
          "variables": {"date": "2022-01-01T00:00:00+00:00"},
          "extensions": {
            "something_simple": "else",
            "something_complex": {
                "nested": "value"
            }
          }
        }));
        insta::assert_yaml_snapshot!(expected_result);
    }

    fn check_deserialization(request: serde_json::Value) -> Request {
        // check that deserialize_from_bytes agrees with Deserialize impl

        let string = serde_json::to_string(&request).expect("could not serialize request");
        let string_deserialized: Request =
            apollo_json::from_str(&string).expect("could not deserialize string");
        let bytes = Bytes::copy_from_slice(string.as_bytes());
        let bytes_deserialized =
            Request::deserialize_from_bytes(&bytes).expect("could not deserialize from bytes");
        assert_eq!(
            string_deserialized, bytes_deserialized,
            "string and bytes deserialization did not match"
        );
        string_deserialized
    }
}
