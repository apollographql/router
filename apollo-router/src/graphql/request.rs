use bytes::Bytes;
use derivative::Derivative;
use serde::de::DeserializeSeed;
use serde::de::Error;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JsonMap;
use serde_json_bytes::Value;

use crate::configuration::BatchingMode;
use crate::json_ext::Object;

/// A GraphQL `Request` used to represent both supergraph and subgraph requests.
#[derive(Clone, Derivative, Serialize, Deserialize, Default)]
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
        skip_serializing_if = "Object::is_empty",
        default,
        deserialize_with = "deserialize_null_default"
    )]
    pub variables: Object,

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
        skip_serializing_if = "Object::is_empty",
        default,
        deserialize_with = "deserialize_null_default"
    )]
    pub extensions: Object,
}

// NOTE: this deserialize helper is used to transform `null` to Default::default()
fn deserialize_null_default<'de, D, T: Default + Deserialize<'de>>(
    deserializer: D,
) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
{
    <Option<T>>::deserialize(deserializer).map(|x| x.unwrap_or_default())
}

fn as_optional_object<E: Error>(value: Value) -> Result<Object, E> {
    use serde::de::Unexpected;

    let exp = "a map or null";
    match value {
        Value::Object(object) => Ok(object),
        // Similar to `deserialize_null_default`:
        Value::Null => Ok(Object::default()),
        Value::Bool(value) => Err(E::invalid_type(Unexpected::Bool(value), &exp)),
        Value::Number(_) => Err(E::invalid_type(Unexpected::Other("a number"), &exp)),
        Value::String(value) => Err(E::invalid_type(Unexpected::Str(value.as_str()), &exp)),
        Value::Array(_) => Err(E::invalid_type(Unexpected::Seq, &exp)),
    }
}

#[buildstructor::buildstructor]
impl Request {
    #[builder(visibility = "pub")]
    /// This is the constructor (or builder) to use when constructing a GraphQL
    /// `Request`.
    ///
    /// The optionality of parameters on this constructor match the runtime
    /// requirements which are necessary to create a valid GraphQL `Request`.
    /// (This contrasts against the `fake_new` constructor which may be more
    /// tolerant to missing properties which are only necessary for testing
    /// purposes.  If you are writing tests, you may want to use `Self::fake_new()`.)
    fn new(
        query: Option<String>,
        operation_name: Option<String>,
        // Skip the `Object` type alias in order to use buildstructor’s map special-casing
        variables: JsonMap<ByteString, Value>,
        extensions: JsonMap<ByteString, Value>,
    ) -> Self {
        Self {
            query,
            operation_name,
            variables,
            extensions,
        }
    }

    #[builder(visibility = "pub")]
    /// This is the constructor (or builder) to use when constructing a **fake**
    /// GraphQL `Request`.  Use `Self::new()` to construct a _real_ request.
    ///
    /// This is offered for testing purposes and it relaxes the requirements
    /// of some parameters to be specified that would be otherwise required
    /// for a real request.  It's usually enough for most testing purposes,
    /// especially when a fully constructed `Request` is difficult to construct.
    /// While today, its paramters have the same optionality as its `new`
    /// counterpart, that may change in future versions.
    fn fake_new(
        query: Option<String>,
        operation_name: Option<String>,
        // Skip the `Object` type alias in order to use buildstructor’s map special-casing
        variables: JsonMap<ByteString, Value>,
        extensions: JsonMap<ByteString, Value>,
    ) -> Self {
        Self {
            query,
            operation_name,
            variables,
            extensions,
        }
    }

    /// Deserialize as JSON from `&Bytes`, avoiding string copies where possible
    pub fn deserialize_from_bytes(data: &Bytes) -> Result<Self, serde_json::Error> {
        let seed = RequestFromBytesSeed(data);
        let mut de = serde_json::Deserializer::from_slice(data);
        seed.deserialize(&mut de)
    }

    /// Convert encoded URL query string parameters (also known as "search
    /// params") into a GraphQL [`Request`].
    ///
    /// An error will be produced in the event that the query string parameters
    /// cannot be turned into a valid GraphQL `Request`.
    pub(crate) fn batch_from_urlencoded_query(
        url_encoded_query: String,
    ) -> Result<Vec<Request>, serde_json::Error> {
        let value: serde_json::Value = serde_urlencoded::from_bytes(url_encoded_query.as_bytes())
            .map_err(serde_json::Error::custom)?;

        Request::process_query_values(&value)
    }

    /// Convert Bytes into a GraphQL [`Request`].
    ///
    /// An error will be produced in the event that the bytes array cannot be
    /// turned into a valid GraphQL `Request`.
    pub(crate) fn batch_from_bytes(bytes: &[u8]) -> Result<Vec<Request>, serde_json::Error> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(serde_json::Error::custom)?;

        Request::process_batch_values(&value)
    }

    fn allocate_result_array(value: &serde_json::Value) -> Vec<Request> {
        match value.as_array() {
            Some(array) => Vec::with_capacity(array.len()),
            None => Vec::with_capacity(1),
        }
    }

    fn process_batch_values(value: &serde_json::Value) -> Result<Vec<Request>, serde_json::Error> {
        let mut result = Request::allocate_result_array(value);

        if value.is_array() {
            tracing::info!(
                histogram.apollo.router.operations.batching.size = result.len() as f64,
                mode = %BatchingMode::BatchHttpLink // Only supported mode right now
            );

            tracing::info!(
                monotonic_counter.apollo.router.operations.batching = 1u64,
                mode = %BatchingMode::BatchHttpLink // Only supported mode right now
            );
            for entry in value
                .as_array()
                .expect("We already checked that it was an array")
            {
                let bytes = serde_json::to_vec(entry)?;
                result.push(Request::deserialize_from_bytes(&bytes.into())?);
            }
        } else {
            let bytes = serde_json::to_vec(value)?;
            result.push(Request::deserialize_from_bytes(&bytes.into())?);
        }
        Ok(result)
    }

    fn process_query_values(value: &serde_json::Value) -> Result<Vec<Request>, serde_json::Error> {
        let mut result = Request::allocate_result_array(value);

        if value.is_array() {
            tracing::info!(
                histogram.apollo.router.operations.batching.size = result.len() as f64,
                mode = "batch_http_link" // Only supported mode right now
            );

            tracing::info!(
                monotonic_counter.apollo.router.operations.batching = 1u64,
                mode = "batch_http_link" // Only supported mode right now
            );
            for entry in value
                .as_array()
                .expect("We already checked that it was an array")
            {
                result.push(Request::process_value(entry)?);
            }
        } else {
            result.push(Request::process_value(value)?)
        }
        Ok(result)
    }

    fn process_value(value: &serde_json::Value) -> Result<Request, serde_json::Error> {
        let operation_name =
            if let Some(serde_json::Value::String(operation_name)) = value.get("operationName") {
                Some(operation_name.clone())
            } else {
                None
            };

        let query = if let Some(serde_json::Value::String(query)) = value.get("query") {
            Some(query.as_str())
        } else {
            None
        };
        let variables: Object = get_from_urlencoded_value(value, "variables")?.unwrap_or_default();
        let extensions: Object =
            get_from_urlencoded_value(value, "extensions")?.unwrap_or_default();

        let request_builder = Self::builder()
            .variables(variables)
            .and_operation_name(operation_name)
            .extensions(extensions);

        let request = if let Some(query_str) = query {
            request_builder.query(query_str).build()
        } else {
            request_builder.build()
        };

        Ok(request)
    }

    /// Convert encoded URL query string parameters (also known as "search
    /// params") into a GraphQL [`Request`].
    ///
    /// An error will be produced in the event that the query string parameters
    /// cannot be turned into a valid GraphQL `Request`.
    pub fn from_urlencoded_query(url_encoded_query: String) -> Result<Request, serde_json::Error> {
        let urldecoded: serde_json::Value =
            serde_urlencoded::from_bytes(url_encoded_query.as_bytes())
                .map_err(serde_json::Error::custom)?;

        Request::process_value(&urldecoded)
    }
}

fn get_from_urlencoded_value<'a, T: Deserialize<'a>>(
    object: &'a serde_json::Value,
    key: &str,
) -> Result<Option<T>, serde_json::Error> {
    if let Some(serde_json::Value::String(byte_string)) = object.get(key) {
        Some(serde_json::from_str(byte_string.as_str())).transpose()
    } else {
        Ok(None)
    }
}

struct RequestFromBytesSeed<'data>(&'data Bytes);

impl<'data, 'de> DeserializeSeed<'de> for RequestFromBytesSeed<'data> {
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

        impl<'data, 'de> serde::de::Visitor<'de> for RequestVisitor<'data> {
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
                    variables: variables.unwrap_or_default(),
                    extensions: extensions.unwrap_or_default(),
                })
            }
        }

        deserializer.deserialize_struct("Request", FIELDS, RequestVisitor(self.0))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use serde_json_bytes::json as bjson;
    use test_log::test;

    use super::*;

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
                .variables(bjson!({ "arg1": "me" }).as_object().unwrap().clone())
                .extensions(bjson!({"extension": 1}).as_object().cloned().unwrap())
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
                .extensions(bjson!({"extension": 1}).as_object().cloned().unwrap())
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
                .extensions(bjson!({"extension": 1}).as_object().cloned().unwrap())
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
        let string_deserialized =
            serde_json::from_str(&string).expect("could not deserialize string");
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
