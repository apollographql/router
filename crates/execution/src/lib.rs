//! Constructs an execution stream from q query plan

use std::fmt::{Debug, Display, Formatter};
use std::pin::Pin;
use std::sync::Arc;

use futures::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::ops::{Deref, DerefMut};
use thiserror::Error;
use typed_builder::TypedBuilder;

#[cfg(feature = "mocks")]
use mockall::{automock, predicate::*};

/// Federated graph fetcher.
pub mod federated;

/// Service registry that uses http_subgraph
pub mod http_service_registry;

/// Subgraph fetcher that uses http.
pub mod http_subgraph;

mod json_utils;
/// Execution context code
mod traverser;

/// A json object
pub type Object = Map<String, Value>;

/// Extensions is an untyped map that can be used to pass extra data to requests and from responses.
pub type Extensions = Object;

/// A list of graphql errors.
pub type Errors = Vec<GraphQLError>;

/// A graph response stream consists of one primary response and any number of patch responses.
pub type GraphQLResponseStream = Pin<Box<dyn Stream<Item = GraphQLResponse> + Send>>;

/// Error types for execution. Note that these are not actually returned to the client, but are
/// instead converted to Json for GraphQLError
#[derive(Error, Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FetchError {
    /// The query plan referenced a service that has not been configured.
    #[error("Query references unknown service '{service}'")]
    ValidationUnknownServiceError {
        /// The service that was unknown.
        service: String,
    },

    /// The variable is missing.
    #[error("Query requires variable '{name}', but it was not provided")]
    ValidationMissingVariable {
        /// Name of the variable.
        name: String,
    },

    /// The request could not be planned.
    #[error("Query could not be planned")]
    ValidationPlanningError {
        /// The failure reason.
        reason: String,
    },

    /// An error when serializing the response.
    #[error("Response was malformed")]
    MalformedResponse {
        /// The reason the serialization failed.
        reason: String,
    },

    /// An error when fetching from a service.
    #[error("Service '{service}' returned no response")]
    SubrequestNoResponse {
        /// The service that returned no response.
        service: String,
    },

    /// An error when serializing a subquery response.
    #[error("Service '{service}' response was malformed")]
    SubrequestMalformedResponse {
        /// The service that responded with the malformed response.
        service: String,

        /// The reason the serialization failed.
        reason: String,
    },

    /// An http error when fetching from a service.
    /// Note that this relates to a transport error and not a GraphQL error.
    #[error("Http fetch failed from: '{service}'")]
    SubrequestHttpError {
        /// The http error code.
        status: Option<u16>,

        /// The service failed.
        service: String,

        /// The reason the fetch failed.
        reason: String,
    },

    /// Field not found in response.
    #[error("Subquery requires field '{field}' but it was not found in the current response")]
    ExecutionFieldNotFound {
        /// The field that is not found.
        field: String,
    },

    /// The content is missing.
    #[error("Missing content at '{path}'")]
    ExecutionMissingContent {
        /// Path to the content.
        path: Path,
    },
}

impl FetchError {
    /// Convert the fetch error to a GraphQL error.
    pub fn to_graphql_error(&self, path: Option<Path>) -> GraphQLError {
        GraphQLError {
            message: self.to_string(),
            locations: Default::default(),
            path: path.unwrap_or_default(),
            extensions: serde_json::to_value(self)
                .unwrap()
                .as_object()
                .unwrap()
                .to_owned(),
        }
    }

    /// Convert the error to an appropriate response.
    pub fn to_response(&self, primary: bool) -> GraphQLResponse {
        if primary {
            self.to_primary()
        } else {
            self.to_patch()
        }
    }

    /// Convert the fetch error to a primary graphql response.
    pub fn to_primary(&self) -> GraphQLResponse {
        GraphQLResponse::Primary(GraphQLPrimaryResponse {
            data: Default::default(),
            has_next: false,
            errors: vec![self.to_graphql_error(None)],
            extensions: Default::default(),
        })
    }

    /// Convert the fetch error to a patch graphql response.
    pub fn to_patch(&self) -> GraphQLResponse {
        // Note that most of the values here will be overwritten when merged into the final response
        // by the traverser. e.g. label, and path.
        GraphQLResponse::Patch(GraphQLPatchResponse {
            label: Default::default(),
            data: Default::default(),
            path: Default::default(),
            has_next: false,
            errors: vec![self.to_graphql_error(None)],
            extensions: Default::default(),
        })
    }
}

/// A GraphQL path element that is composes of strings or numbers.
/// e.g `/book/3/name`
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PathElement {
    /// An index path element.
    Index(usize),

    /// A key path element.
    Key(String),

    /// A path element that given an array will flatmap the content.
    Flatmap,
}

/// A path into the result document. This can be composed of strings and numbers
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Path {
    path: Vec<PathElement>,
}

impl Deref for Path {
    type Target = Vec<PathElement>;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl DerefMut for Path {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.path
    }
}

impl Path {
    fn new(path: &[PathElement]) -> Path {
        Path { path: path.into() }
    }
    fn parse(path: impl Into<String>) -> Path {
        Path {
            path: path
                .into()
                .split('/')
                .map(|e| match (e, e.parse::<usize>()) {
                    (_, Ok(index)) => PathElement::Index(index),
                    (s, _) if s == "@" => PathElement::Flatmap,
                    (s, _) => PathElement::Key(s.to_string()),
                })
                .collect(),
        }
    }

    fn empty() -> Path {
        Path {
            path: Default::default(),
        }
    }

    fn parent(&self) -> Path {
        let mut path = self.path.to_owned();
        path.pop();
        Path { path }
    }

    fn append(&mut self, path: &Path) {
        self.path.append(&mut path.path.to_owned());
    }
}

impl Display for Path {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            self.path
                .iter()
                .map(|e| match e {
                    PathElement::Index(index) => index.to_string(),
                    PathElement::Key(key) => key.into(),
                    PathElement::Flatmap => "@".into(),
                })
                .collect::<Vec<String>>()
                .join("/")
                .as_str(),
        )
    }
}

/// A graphql request.
/// Used for federated and subgraph queries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
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
    pub extensions: Extensions,
}

/// A graphql primary response.
/// Used for federated and subgraph queries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphQLPrimaryResponse {
    /// The response data.
    pub data: Object,

    /// The optional indicator that there may be more data in the form of a patch response.
    #[serde(skip_serializing_if = "bool::to_owned", default)]
    pub has_next: bool,

    /// The optional graphql errors encountered.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub errors: Errors,

    /// The optional graphql extensions.
    #[serde(skip_serializing_if = "Object::is_empty", default)]
    pub extensions: Extensions,
}

/// A graphql patch response .
/// Used for federated and subgraph queries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphQLPatchResponse {
    /// The label that was passed to the defer or stream directive for this patch.
    pub label: String,

    /// The data to merge into the response.
    pub data: Object,

    /// The path that the data should be merged at.
    pub path: Path,

    /// An indicator if there is potentially more data to fetch.
    pub has_next: bool,

    /// The optional errors encountered for this patch.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub errors: Errors,

    /// The optional graphql extensions.
    #[serde(skip_serializing_if = "Object::is_empty", default)]
    pub extensions: Extensions,
}

/// A GraphQL error.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphQLError {
    /// The error message.
    pub message: String,

    /// The locations of the error from the originating request.
    pub locations: Vec<Location>,

    /// The path of the error.
    pub path: Path,

    /// The optional graphql extensions.
    #[serde(default, skip_serializing_if = "Object::is_empty")]
    pub extensions: Extensions,
}

/// A location in the request that triggered a graphql error.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    /// The line number.
    pub line: i32,

    /// The column number.
    pub column: i32,
}

/// A GraphQL response.
/// A response stream will typically be composed of a single primary and zero or more patches.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GraphQLResponse {
    /// The first item in a stream of responses will always be a primary response.
    Primary(GraphQLPrimaryResponse),

    /// Subsequent responses will always be a patch response.
    Patch(GraphQLPatchResponse),
}

impl GraphQLResponse {
    /// Return as a primary response. Panics if not the right type, so should only be used in testing.
    pub fn primary(self) -> GraphQLPrimaryResponse {
        if let GraphQLResponse::Primary(primary) = self {
            primary
        } else {
            panic!("Not a primary response")
        }
    }

    /// Return as a patch response. Panics if not the right type, so should only be used in testing.
    pub fn patch(self) -> GraphQLPatchResponse {
        if let GraphQLResponse::Patch(patch) = self {
            patch
        } else {
            panic!("Not patch response")
        }
    }
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
pub trait GraphQLFetcher: Send + Sync + Debug {
    /// Constructs a stream of responses.
    #[must_use = "streams do nothing unless polled"]
    fn stream(&self, request: GraphQLRequest) -> GraphQLResponseStream;
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

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
                has_next: Default::default(),
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
                errors: vec!(GraphQLError {
                    message: "Name for character with ID 1002 could not be fetched.".into(),
                    locations: vec!(Location { line: 6, column: 7 }),
                    path: Path::parse("hero/heroFriends/1/name"),
                    extensions: json!({
                        "error-extension": 5,
                    })
                    .as_object()
                    .cloned()
                    .unwrap()
                }),
                extensions: json!({
                    "response-extension": 3,
                })
                .as_object()
                .cloned()
                .unwrap()
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
                path: Path::parse("hero/heroFriends/1/name"),
                has_next: true,
                errors: vec!(GraphQLError {
                    message: "Name for character with ID 1002 could not be fetched.".into(),
                    locations: vec!(Location { line: 6, column: 7 }),
                    path: Path::parse("hero/heroFriends/1/name"),
                    extensions: json!({
                        "error-extension": 5,
                    })
                    .as_object()
                    .cloned()
                    .unwrap()
                }),
                extensions: json!({
                    "response-extension": 3,
                })
                .as_object()
                .cloned()
                .unwrap()
            }
        );
    }
}
