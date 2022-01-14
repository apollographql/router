use crate::prelude::graphql::*;
use bytes::Bytes;
use derivative::Derivative;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use typed_builder::TypedBuilder;

/// A graphql request.
/// Used for federated and subgraph queries.
#[derive(Clone, Derivative, Serialize, Deserialize, TypedBuilder)]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
#[derivative(Debug, PartialEq)]
pub struct Request {
    /// The graphql query.
    pub query: String,

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
            .map(|s| s.as_str().to_string())
            .unwrap_or_default();
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
}
