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

/// the parsed graphql Request, HTTP headers and contextual data for extensions
pub struct RouterRequest {
    /// The original request
    pub frontend_request: http::Request<Request>,

    /// Context for extension
    pub context: Object,
}

pub struct PlannedRequest {
    ///riginal request
    pub frontend_request: Arc<http::Request<Request>>,

    // hiding this one for now
    //pub query_plan: QueryPlan,

    // Cloned from RouterRequest
    pub context: Object,
}

pub struct SubgraphRequest {
    pub service_name: String,

    // The request to make to subgraphs
    pub backend_request: http::Request<Request>,

    // Subgraph extensions have access to the frontend request
    // but cannot modify it
    pub frontend_request: Arc<http::Request<Request>>,

    // Cloned from PlannedRequest
    pub context: Object,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use test_log::test;

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
