use derivative::Derivative;
use serde::de::Error;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JsonMap;
use serde_json_bytes::Value;

use crate::json_ext::Object;

/// A GraphQL `Request` used to represent both supergraph and subgraph requests.
#[derive(Clone, Derivative, Serialize, Deserialize, Default)]
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
    #[serde(skip_serializing_if = "Object::is_empty", default)]
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

    /// Convert encoded URL query string parameters (also known as "search
    /// params") into a GraphQL [`Request`].
    ///
    /// An error will be produced in the event that the query string parameters
    /// cannot be turned into a valid GraphQL `Request`.
    pub fn from_urlencoded_query(url_encoded_query: String) -> Result<Request, serde_json::Error> {
        let urldecoded: serde_json::Value =
            serde_urlencoded::from_bytes(url_encoded_query.as_bytes())
                .map_err(serde_json::Error::custom)?;

        let operation_name = if let Some(serde_json::Value::String(operation_name)) =
            urldecoded.get("operationName")
        {
            Some(operation_name.clone())
        } else {
            None
        };

        let query = if let Some(serde_json::Value::String(query)) = urldecoded.get("query") {
            Some(query.as_str())
        } else {
            None
        };
        let variables: Object = get_from_urldecoded(&urldecoded, "variables")?.unwrap_or_default();
        let extensions: Object =
            get_from_urldecoded(&urldecoded, "extensions")?.unwrap_or_default();

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
}

fn get_from_urldecoded<'a, T: Deserialize<'a>>(
    object: &'a serde_json::Value,
    key: &str,
) -> Result<Option<T>, serde_json::Error> {
    if let Some(serde_json::Value::String(byte_string)) = object.get(key) {
        Some(serde_json::from_str(byte_string.as_str())).transpose()
    } else {
        Ok(None)
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
        })
        .to_string();
        println!("data: {data}");
        let result = serde_json::from_str::<Request>(data.as_str());
        println!("result: {result:?}");
        assert_eq!(
            result.unwrap(),
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
        let result = serde_json::from_str::<Request>(
            json!(
            {
              "query": "query aTest($arg1: String!) { test(who: $arg1) }",
              "operationName": "aTest",
              "extensions": {"extension": 1}
            })
            .to_string()
            .as_str(),
        );
        assert_eq!(
            result.unwrap(),
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
        let result = serde_json::from_str::<Request>(
            json!(
            {
              "query": "query aTest($arg1: String!) { test(who: $arg1) }",
              "operationName": "aTest",
              "variables": null,
              "extensions": {"extension": 1}
            })
            .to_string()
            .as_str(),
        );
        assert_eq!(
            result.unwrap(),
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

        let expected_result = serde_json::from_str::<Request>(
            json!(
            {
              "query": "{ topProducts { upc name reviews { id product { name } author { id name } } } }",
              "extensions": {
                  "persistedQuery": {
                      "version": 1,
                      "sha256Hash": "20a101de18d4a9331bfc4ccdfef33cc735876a689490433570f17bdd4c0bad3f"
                  }
                }
            })
            .to_string()
            .as_str(),
        ).unwrap();

        let req = Request::from_urlencoded_query(query_string).unwrap();

        assert_eq!(expected_result, req);
    }

    #[test]
    fn from_urlencoded_query_with_variables_works() {
        let query_string = "query=%7B+topProducts+%7B+upc+name+reviews+%7B+id+product+%7B+name+%7D+author+%7B+id+name+%7D+%7D+%7D+%7D&variables=%7B%22date%22%3A%222022-01-01T00%3A00%3A00%2B00%3A00%22%7D&extensions=%7B+%22persistedQuery%22+%3A+%7B+%22version%22+%3A+1%2C+%22sha256Hash%22+%3A+%2220a101de18d4a9331bfc4ccdfef33cc735876a689490433570f17bdd4c0bad3f%22+%7D+%7D".to_string();

        let expected_result = serde_json::from_str::<Request>(
            json!(
            {
              "query": "{ topProducts { upc name reviews { id product { name } author { id name } } } }",
              "variables": {"date": "2022-01-01T00:00:00+00:00"},
              "extensions": {
                  "persistedQuery": {
                      "version": 1,
                      "sha256Hash": "20a101de18d4a9331bfc4ccdfef33cc735876a689490433570f17bdd4c0bad3f"
                  }
                }
            })
            .to_string()
            .as_str(),
        ).unwrap();

        let req = Request::from_urlencoded_query(query_string).unwrap();

        assert_eq!(expected_result, req);
    }
}
