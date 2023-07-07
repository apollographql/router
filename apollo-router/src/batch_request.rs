use derivative::Derivative;
use serde::de::Error;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JsonMap;
use serde_json_bytes::Value;

use crate::json_ext::Object;
use serde_json::Value;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::option::Option::None;

/// A GraphQL `Request` used to represent both supergraph and subgraph requests.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum Request {
    /// Represents a single GraphQL request.
    Single(SingleRequest),
    /// Represents a batch of multiple GraphQL requests.
    Batch(Vec<SingleRequest>),
}

impl Request {
    /// Convert encoded URL query string parameters (also known as "search
    /// params") into a GraphQL [`Request`].
    ///
    /// An error will be produced in the event that the query string parameters
    /// cannot be turned into a valid GraphQL `Request`.
    pub fn from_urlencoded_query(url_encoded_query: String) -> Result<Request, serde_json::Error> {
        let urldecoded: serde_json::Value = serde_urlencoded::from_bytes(url_encoded_query.as_bytes())
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
        let extensions: Object = get_from_urldecoded(&urldecoded, "extensions")?.unwrap_or_default();

        let request_builder = SingleRequest::builder()
            .variables(variables)
            .and_operation_name(operation_name)
            .extensions(extensions);

        let request = if let Some(query_str) = query {
            request_builder.query(query_str).build()
        } else {
            request_builder.build()
        };

        Ok(Request::Single(request))
    }
}

/// Represents a single GraphQL request.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SingleRequest {
    pub query: Option<String>,
    pub operation_name: Option<String>,
    #[serde(skip_serializing_if = "HashMap::is_empty", default)]
    pub variables: HashMap<String, Value>,
    #[serde(skip_serializing_if = "HashMap::is_empty", default)]
    pub extensions: HashMap<String, Value>,
}

impl SingleRequest {
    #[builder::builder]
    /// This is the constructor (or builder) to use when constructing a GraphQL
    /// `SingleRequest`.
    ///
    /// The optionality of parameters on this constructor match the runtime
    /// requirements which are necessary to create a valid GraphQL `SingleRequest`.
    /// (This contrasts against the `fake_new` constructor which may be more
    /// tolerant to missing properties which are only necessary for testing
    /// purposes.  If you are writing tests, you may want to use `Self::fake_new()`.)
    pub fn new(
        query: Option<String>,
        operation_name: Option<String>,
        #[serde(skip_serializing_if = "HashMap::is_empty")] variables: HashMap<String, Value>,
        #[serde(skip_serializing_if = "HashMap::is_empty")] extensions: HashMap<String, Value>,
    ) -> Self {
        Self {
            query,
            operation_name,
            variables,
            extensions,
        }
    }

    /// This is the constructor (or builder) to use when constructing a **fake**
    /// GraphQL `SingleRequest`. Use `Self::new()` to construct a _real_ request.
    ///
    /// This is offered for testing purposes and it relaxes the requirements
    /// of some parameters to be specified that would be otherwise required
    /// for a real request. It's usually enough for most testing purposes,
    /// especially when a fully constructed `SingleRequest` is difficult to construct.
    /// While today, its parameters have the same optionality as its `new`
    /// counterpart, that may change in future versions.
    pub fn fake_new(
        query: Option<String>,
        operation_name: Option<String>,
        #[serde(skip_serializing_if = "HashMap::is_empty")] variables: HashMap<String, Value>,
        #[serde(skip_serializing_if = "HashMap::is_empty")] extensions: HashMap<String, Value>,
    ) -> Self {
        Self {
            query,
            operation_name,
            variables,
            extensions,
        }
    }
}

/// A type alias for HashMap<String, Value>.
type Object = HashMap<String, Value>;

/// Helper function to extract a JSON object from the urldecoded value.
fn get_from_urldecoded(urldecoded: &serde_json::Value, key: &str) -> Result<Option<Object>, serde_json::Error> {
    if let Some(serde_json::Value::Object(obj)) = urldecoded.get(key) {
        Ok(Some(obj.clone()))
    } else {
        Ok(None)
    }
}


// fn get_from_urldecoded<'a, T: Deserialize<'a>>(
//     object: &'a serde_json::Value,
//     key: &str,
// ) -> Result<Option<T>, serde_json::Error> {
//     if let Some(serde_json::Value::String(byte_string)) = object.get(key) {
//         Some(serde_json::from_str(byte_string.as_str())).transpose()
//     } else {
//         Ok(None)
//     }
// }

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
        // println!("data: {data}");
        let result = serde_json::from_str::<Request>(data.as_str());
        // println!("result: {result:?}");
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
