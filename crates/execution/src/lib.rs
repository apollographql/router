//! Constructs an execution stream from q query plan

use apollo_json_ext::prelude::*;
use derivative::Derivative;
use futures::prelude::*;
#[cfg(feature = "mocks")]
use mockall::{automock, predicate::*};
use query_planner::QueryPlannerError;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::pin::Pin;
use std::sync::Arc;
use typed_builder::TypedBuilder;

mod error;
/// Federated graph fetcher.
pub mod federated;
/// Service registry that uses http_subgraph
pub mod http_service_registry;
/// Subgraph fetcher that uses http.
pub mod http_subgraph;
mod response;

pub use error::*;
pub use response::*;

/// A graph response stream consists of one primary response and any number of patch responses.
pub type GraphQLResponseStream = Pin<Box<dyn Stream<Item = GraphQLResponse> + Send>>;

/// A graphql request.
/// Used for federated and subgraph queries.
#[derive(Clone, Derivative, Serialize, Deserialize, TypedBuilder)]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
#[derivative(Debug, PartialEq)]
pub struct GraphQLRequest {
    /// The graphql query.
    pub query: String,

    /// The optional graphql operation.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[builder(default)]
    pub operation_name: Option<String>,

    /// The optional variables in the form of a json object.
    #[serde(skip_serializing_if = "Object::is_empty", default)]
    #[builder(default)]
    pub variables: Arc<Object>,

    /// Graphql extensions.
    #[serde(skip_serializing_if = "Object::is_empty", default)]
    #[builder(default)]
    pub extensions: Object,
}

/// Maintains a map of services to fetchers.
pub trait ServiceRegistry: Send + Sync + Debug {
    /// Get a fetcher for a service.
    fn get(&self, service: &str) -> Option<&(dyn GraphQLFetcher)>;

    /// Get a fetcher for a service.
    fn has(&self, service: &str) -> bool;
}

/// A fetcher is responsible for turning a graphql request into a stream of responses.
///
/// The goal of this trait is to hide the implementation details of retching a stream of graphql responses.
/// We can then create multiple implementations that can be plugged into federation.
#[cfg_attr(feature = "mocks", automock)]
pub trait GraphQLFetcher: Send + Sync {
    /// Constructs a stream of responses.
    #[must_use = "streams do nothing unless polled"]
    fn stream(&self, request: GraphQLRequest) -> GraphQLResponseStream;
}

impl From<QueryPlannerError> for FetchError {
    fn from(err: QueryPlannerError) -> Self {
        FetchError::ValidationPlanningError {
            reason: err.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use static_assertions::*;

    assert_obj_safe!(ServiceRegistry);

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
            GraphQLRequest::builder()
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
            GraphQLResponse::builder()
                .data(json!({
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
                }))
                .errors(vec![GraphQLError {
                    message: "Name for character with ID 1002 could not be fetched.".into(),
                    locations: vec!(Location { line: 6, column: 7 }),
                    path: Path::from("hero/heroFriends/1/name"),
                    extensions: json!({
                        "error-extension": 5,
                    })
                    .as_object()
                    .cloned()
                    .unwrap()
                }])
                .extensions(
                    json!({
                        "response-extension": 3,
                    })
                    .as_object()
                    .cloned()
                    .unwrap()
                )
                .build()
        );
    }

    #[test]
    fn test_patch_response() {
        let result = serde_json::from_str::<GraphQLResponse>(
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
            GraphQLResponse::builder()
                .label("part".to_owned())
                .data(json!({
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
                }))
                .path(Path::from("hero/heroFriends/1/name"))
                .has_next(true)
                .errors(vec![GraphQLError {
                    message: "Name for character with ID 1002 could not be fetched.".into(),
                    locations: vec!(Location { line: 6, column: 7 }),
                    path: Path::from("hero/heroFriends/1/name"),
                    extensions: json!({
                        "error-extension": 5,
                    })
                    .as_object()
                    .cloned()
                    .unwrap()
                }])
                .extensions(
                    json!({
                        "response-extension": 3,
                    })
                    .as_object()
                    .cloned()
                    .unwrap()
                )
                .build()
        );
    }
}
