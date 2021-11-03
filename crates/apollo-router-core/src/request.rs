use crate::prelude::graphql::*;
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
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[builder(default)]
    pub variables: Option<Arc<Object>>,

    ///  extensions.
    #[serde(skip_serializing_if = "Object::is_empty", default)]
    #[builder(default)]
    pub extensions: Object,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use test_env_log::test;

    #[test]
    fn test_request() {
        let result = serde_json::from_str::<Request>(
            json!(
            {
              "query": "query aTest($arg1: String!) { test(who: $arg1) }",
              "operationName": "aTest",
              "variables": { "arg1": "me" },
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
                .variables(Arc::new(
                    json!({ "arg1": "me" }).as_object().unwrap().clone()
                ))
                .extensions(json!({"extension": 1}).as_object().cloned().unwrap())
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
                .extensions(json!({"extension": 1}).as_object().cloned().unwrap())
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
                .extensions(json!({"extension": 1}).as_object().cloned().unwrap())
                .build()
        );
    }
}
