//! Constructs an execution stream from q query plan

/// Federated graph fetcher.
pub mod federated;

/// Subgraph fetcher that uses http.
pub mod http_subgraph;

use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::pin::Pin;
use thiserror::Error;

/// A json object
pub type Object = Map<String, Value>;

/// A path for an error. This can be composed of strings and numbers
pub type Path = Vec<PathElement>;

/// Extensions is an untyped map that can be used to pass extra data to requests and from responses.
pub type Extensions = Option<Object>;

/// A list of graphql errors.
pub type Errors = Option<Vec<GraphQLError>>;

/// Error types for QueryPlanner
#[derive(Error, Debug)]
pub enum FetchError {
    /// An error when fetching from a service.
    #[error("The service has errors {service}")]
    ServiceError {
        /// The service that caused this error
        service: String,
        /// The source error
        source: Box<dyn std::error::Error>,
    },

    /// The response was malformed
    #[error("The request had errors {source}")]
    RequestError {
        /// The source error
        source: Box<dyn std::error::Error>,
    },
}

#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
#[serde(untagged)]
/// A GraphQL path element that is composes of strings or numbers.
/// e.g `/book/3/name`
pub enum PathElement {
    /// An integer path element.
    Number(i32),

    /// A string path element.
    String(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// A graphql request.
/// Used for federated and subgraph queries.
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
/// A graphql primary response.
/// Used for federated and subgraph queries.
pub struct GraphQLPrimaryResponse {
    data: Object,
    #[serde(skip_serializing_if = "Option::is_none")]
    has_next: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    errors: Errors,
    #[serde(skip_serializing_if = "Option::is_none")]
    extensions: Extensions,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// A graphql patch response .
/// Used for federated and subgraph queries.
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
/// A GraphQL error.
pub struct GraphQLError {
    message: String,
    locations: Vec<Location>,
    path: Path,
    extensions: Extensions,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// A location in a file in a graphql error.
pub struct Location {
    line: i32,
    column: i32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// A GraphQL response.
/// A response stream will typically be composed of a single primary and zero or more patches.
pub enum GraphQLResponse {
    /// The first item in a stream of responses will always be a primary response.
    Primary(GraphQLPrimaryResponse),

    /// Subsequent responses will always be a patch response.
    Patch(GraphQLPatchResponse),
}

/// Maintains a map of services to stream builder for querying the service.
/// Used for subgraph queries.
trait ServiceManager {
    fn get(&self, service: String) -> &dyn GraphQLFetcher;
}

/// A graph response stream consists of one primary response and any number of patch responses.
pub type GraphQLResponseStream = Pin<Box<dyn Stream<Item = Result<GraphQLResponse, FetchError>>>>;

/// A fetcher is responsible for turning a graphql request into a stream of responses.
/// The goal of this trait is to hide the implementation details of retching a stream of graphql responses.
/// We can then create multiple implementations that cab be plugged in to federation.
trait GraphQLFetcher {
    /// Constructs a stream of responses.
    #[must_use = "streams do nothing unless polled"]
    fn stream(&self, request: &GraphQLRequest) -> GraphQLResponseStream;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        let result = serde_json::from_str::<GraphQLPrimaryResponse>(
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
            GraphQLPrimaryResponse {
                has_next: None,
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
                        PathElement::String("hero".to_owned()),
                        PathElement::String("heroFriends".to_owned()),
                        PathElement::Number(1),
                        PathElement::String("name".to_owned())
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
                    PathElement::String("hero".to_owned()),
                    PathElement::String("heroFriends".to_owned()),
                    PathElement::Number(1),
                    PathElement::String("name".to_owned())
                ),
                has_next: true,
                errors: Some(vec!(GraphQLError {
                    message: "Name for character with ID 1002 could not be fetched.".to_owned(),
                    locations: vec!(Location { line: 6, column: 7 }),
                    path: vec!(
                        PathElement::String("hero".to_owned()),
                        PathElement::String("heroFriends".to_owned()),
                        PathElement::Number(1),
                        PathElement::String("name".to_owned())
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
