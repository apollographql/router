use crate::prelude::graphql::*;
use bytes::Bytes;
use derivative::Derivative;
use serde::{
    de::{DeserializeOwned, Error},
    Deserialize, Serialize,
};
use std::sync::Arc;
use typed_builder::TypedBuilder;
use urlencoding::decode;

/// A graphql request.
/// Used for federated and subgraph queries.
#[derive(Clone, Derivative, Serialize, Deserialize, TypedBuilder)]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
#[derivative(Debug, PartialEq)]
pub struct Request {
    /// The graphql query.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub query: Option<String>,

    /// The optional graphql operation.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[builder(default)]
    pub operation_name: Option<String>,

    /// The optional variables in the form of a json object.
    #[serde(
        skip_serializing_if = "Object::is_empty",
        default,
        deserialize_with = "deserialize_null_default"
    )]
    #[builder(default)]
    pub variables: Arc<Object>,

    ///  extensions.
    #[serde(skip_serializing_if = "Object::is_empty", default)]
    #[builder(default)]
    pub extensions: Object,
}

pub fn from_urlencoded_query(url_encoded_query: String) -> Result<Request, serde_json::Error> {
    // decode percent encoded string
    // from the docs `Unencoded `+` is preserved literally, and _not_ changed to a space.`,
    // so let's do it I guess
    let query = url_encoded_query.replace('+', " ");
    let decoded_string = decode(query.as_str()).unwrap();
    let urldecoded: serde_json::Value =
        serde_urlencoded::from_str(&decoded_string).map_err(|e| serde_json::Error::custom(e))?;

    let operation_name = get(&urldecoded, "operationName").unwrap();
    let query = if let Some(serde_json::Value::String(query)) = urldecoded.get("query") {
        Some(query.clone())
    } else {
        None
    };
    let variables = Arc::new(get(&urldecoded, "variables")?.unwrap_or_default());
    let extensions: Object = get(&urldecoded, "extensions").unwrap().unwrap_or_default();

    Ok(Request::builder()
        .query(query)
        .variables(variables)
        .operation_name(operation_name)
        .extensions(extensions)
        .build())
}

fn get<T: DeserializeOwned>(
    object: &serde_json::Value,
    key: &str,
) -> Result<Option<T>, serde_json::Error> {
    if let Some(serde_json::Value::String(byte_string)) = object.get(key) {
        Some(serde_json::from_str(byte_string.as_str())).transpose()
    } else {
        Ok(None)
    }
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

impl Request {
    pub fn from_bytes(b: Bytes) -> Result<Request, serde_json::error::Error> {
        let value = Value::from_bytes(b)?;
        let mut object = ensure_object!(value).map_err(serde::de::Error::custom)?;

        let variables = extract_key_value_from_object!(object, "variables", Value::Object(o) => o)
            .map_err(serde::de::Error::custom)?
            .unwrap_or_default();
        let extensions =
            extract_key_value_from_object!(object, "extensions", Value::Object(o) => o)
                .map_err(serde::de::Error::custom)?
                .unwrap_or_default();
        let query = extract_key_value_from_object!(object, "query", Value::String(s) => s)
            .map_err(serde::de::Error::custom)?
            .map(|s| s.as_str().to_string());

        let operation_name =
            extract_key_value_from_object!(object, "operation_name", Value::String(s) => s)
                .map_err(serde::de::Error::custom)?
                .map(|s| s.as_str().to_string());

        Ok(Request {
            query,
            operation_name,
            variables: Arc::new(variables),
            extensions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use serde_json_bytes::json as bjson;
    use test_log::test;

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
        println!("data: {}", data);
        let result = serde_json::from_str::<Request>(data.as_str());
        println!("result: {:?}", result);
        assert_eq!(
            result.unwrap(),
            Request::builder()
                .query("query aTest($arg1: String!) { test(who: $arg1) }".to_owned())
                .operation_name(Some("aTest".to_owned()))
                .variables(Arc::new(
                    bjson!({ "arg1": "me" }).as_object().unwrap().clone()
                ))
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
                .operation_name(Some("aTest".to_owned()))
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
                .query("query aTest($arg1: String!) { test(who: $arg1) }".to_owned())
                .operation_name(Some("aTest".to_owned()))
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

        let req = from_urlencoded_query(query_string).unwrap();

        assert_eq!(expected_result, req);
    }
}
