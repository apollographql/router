//! Constructs an execution stream from q query plan

mod subgraph;

use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub type Object = Map<String, Value>;
pub type Path = Vec<Value>;
pub type Extensions = Option<Object>;
pub type Errors = Option<Vec<GraphQLError>>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphQLRequest {
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    variables: Option<Object>,
    #[serde(skip_serializing_if = "Option::is_none")]
    extensions: Extensions,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphQLResponse {
    data: Object,
    #[serde(skip_serializing_if = "Option::is_none")]
    errors: Errors,
    #[serde(skip_serializing_if = "Option::is_none")]
    extensions: Extensions,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphQLPatchResponse {
    label: String,
    data: Object,
    path: Path,
    has_next: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    errors: Errors,
    #[serde(skip_serializing_if = "Option::is_none")]
    extensions: Extensions,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphQLError {
    message: String,
    locations: Vec<Location>,
    path: Path,
    extensions: Extensions,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    line: i32,
    column: i32,
}

pub enum GraphQLResponseItem {
    Primary(GraphQLResponse),
    Patch(GraphQLPatchResponse),
}

trait ExecutorManager {
    fn get(&self, ur: String) -> &dyn Executor;
}

trait Executor {
    fn execute(&self, request: &GraphQLRequest) -> dyn Stream<Item = GraphQLResponseItem>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Number, Value};

    #[test]
    fn test_request() {
        let result = serde_json::from_str::<GraphQLRequest>(
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
            GraphQLRequest {
                query: "query aTest($arg1: String!) { test(who: $arg1) }".to_owned(),
                operation_name: Some("aTest".to_owned()),
                variables: json!({ "arg1": "me" }).as_object().cloned(),
                extensions: json!({"extension": 1}).as_object().cloned()
            },
        );
    }

    #[test]
    fn test_response() {
        let result = serde_json::from_str::<GraphQLResponse>(
            json!(
            {
              "errors": [
                {
                  "message": "Name for character with ID 1002 could not be fetched.",
                  "locations": [{ "line": 6, "column": 7 }],
                  "path": ["hero", "heroFriends", 1, "name"],
                  "extensions": {
                    "error-extension": 5,
                  }
                }
              ],
              "data": {
                "hero": {
                  "name": "R2-D2",
                  "heroFriends": [
                    {
                      "id": "1000",
                      "name": "Luke Skywalker"
                    },
                    {
                      "id": "1002",
                      "name": null
                    },
                    {
                      "id": "1003",
                      "name": "Leia Organa"
                    }
                  ]
                }
              },
              "extensions": {
                "response-extension": 3,
              }
            })
            .to_string()
            .as_str(),
        );
        assert_eq!(
            result.unwrap(),
            GraphQLResponse {
                data: json!({
                  "hero": {
                    "name": "R2-D2",
                    "heroFriends": [
                      {
                        "id": "1000",
                        "name": "Luke Skywalker"
                      },
                      {
                        "id": "1002",
                        "name": null
                      },
                      {
                        "id": "1003",
                        "name": "Leia Organa"
                      }
                    ]
                  }
                })
                .as_object()
                .cloned()
                .unwrap(),
                errors: Some(vec!(GraphQLError {
                    message: "Name for character with ID 1002 could not be fetched.".to_owned(),
                    locations: vec!(Location { line: 6, column: 7 }),
                    path: vec!(
                        Value::String("hero".to_owned()),
                        Value::String("heroFriends".to_owned()),
                        Value::Number(Number::from(1)),
                        Value::String("name".to_owned())
                    ),
                    extensions: json!({
                        "error-extension": 5,
                    })
                    .as_object()
                    .cloned()
                })),
                extensions: json!({
                    "response-extension": 3,
                })
                .as_object()
                .cloned()
            }
        );
    }

    #[test]
    fn test_patch_response() {
        let result = serde_json::from_str::<GraphQLPatchResponse>(
            json!(
            {
              "label": "part",
              "hasNext": true,
              "path": ["hero", "heroFriends", 1, "name"],
              "errors": [
                {
                  "message": "Name for character with ID 1002 could not be fetched.",
                  "locations": [{ "line": 6, "column": 7 }],
                  "path": ["hero", "heroFriends", 1, "name"],
                  "extensions": {
                    "error-extension": 5,
                  }
                }
              ],
              "data": {
                "hero": {
                  "name": "R2-D2",
                  "heroFriends": [
                    {
                      "id": "1000",
                      "name": "Luke Skywalker"
                    },
                    {
                      "id": "1002",
                      "name": null
                    },
                    {
                      "id": "1003",
                      "name": "Leia Organa"
                    }
                  ]
                }
              },
              "extensions": {
                "response-extension": 3,
              }
            })
            .to_string()
            .as_str(),
        );
        assert_eq!(
            result.unwrap(),
            GraphQLPatchResponse {
                label: "part".to_owned(),
                data: json!({
                  "hero": {
                    "name": "R2-D2",
                    "heroFriends": [
                      {
                        "id": "1000",
                        "name": "Luke Skywalker"
                      },
                      {
                        "id": "1002",
                        "name": null
                      },
                      {
                        "id": "1003",
                        "name": "Leia Organa"
                      }
                    ]
                  }
                })
                .as_object()
                .cloned()
                .unwrap(),
                path: vec!(
                    Value::String("hero".to_owned()),
                    Value::String("heroFriends".to_owned()),
                    Value::Number(Number::from(1)),
                    Value::String("name".to_owned())
                ),
                has_next: true,
                errors: Some(vec!(GraphQLError {
                    message: "Name for character with ID 1002 could not be fetched.".to_owned(),
                    locations: vec!(Location { line: 6, column: 7 }),
                    path: vec!(
                        Value::String("hero".to_owned()),
                        Value::String("heroFriends".to_owned()),
                        Value::Number(Number::from(1)),
                        Value::String("name".to_owned())
                    ),
                    extensions: json!({
                        "error-extension": 5,
                    })
                    .as_object()
                    .cloned()
                })),
                extensions: json!({
                    "response-extension": 3,
                })
                .as_object()
                .cloned()
            }
        );
    }
}
