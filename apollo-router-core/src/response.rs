use crate::prelude::graphql::{self, *};
use bytes::Bytes;
use futures::prelude::*;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use typed_builder::TypedBuilder;

/// A graph response stream consists of one primary response and any number of patch responses.
pub type ResponseStream = Pin<Box<dyn futures::Stream<Item = Response> + Send>>;

/// A graphql primary response.
/// Used for federated and subgraph queries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct Response {
    /// The label that was passed to the defer or stream directive for this patch.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[builder(default)]
    pub label: Option<String>,

    /// The response data.
    #[serde(skip_serializing_if = "skip_data_if", default)]
    #[builder(default = Value::Object(Default::default()))]
    pub data: Value,

    /// The path that the data should be merged at.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[builder(default)]
    pub path: Option<Path>,

    /// The optional indicator that there may be more data in the form of a patch response.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[builder(default)]
    pub has_next: Option<bool>,

    /// The optional graphql errors encountered.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    #[builder(default)]
    pub errors: Vec<Error>,

    /// The optional graphql extensions.
    #[serde(skip_serializing_if = "Object::is_empty", default)]
    #[builder(default)]
    pub extensions: Object,
}

fn skip_data_if(value: &Value) -> bool {
    match value {
        Value::Object(o) => o.is_empty(),
        Value::Null => true,
        _ => false,
    }
}

impl Response {
    pub fn is_primary(&self) -> bool {
        self.path.is_none()
    }

    /// append_errors default the errors `path` with the one provided.
    pub fn append_errors(&mut self, errors: &mut Vec<Error>) {
        self.errors.append(errors)
    }

    pub fn from_bytes(service_name: &str, b: Bytes) -> Result<Response, graphql::FetchError> {
        let value = Value::from_bytes(b).map_err(|error| {
            graphql::FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: error.to_string(),
            }
        })?;

        let mut object = match value {
            Value::Object(o) => o,
            _ => {
                return Err(graphql::FetchError::SubrequestMalformedResponse {
                    service: service_name.to_string(),
                    reason: "expected a JSON object".to_string(),
                })
            }
        };

        // FIXME: we should check that if label is there, it should be a string
        let label = object
            .get("label")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let data = object.remove("data").unwrap_or_default();

        let path = object
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| s.into());
        let has_next = object.get("has_next").and_then(|v| v.as_bool());

        let errors = match object.remove("errors") {
            Some(Value::Array(v)) => {
                let res: Result<Vec<Error>, FetchError> = v
                    .into_iter()
                    .map(|v| Error::from_value(service_name, v))
                    .collect();
                res?
            }
            _ => Vec::new(),
        };

        let extensions = match object.remove("extensions") {
            Some(Value::Object(o)) => o,
            _ => Object::new(),
        };

        Ok(Response {
            label,
            data,
            path,
            has_next,
            errors,
            extensions,
        })
    }
}

impl From<Response> for ResponseStream {
    fn from(response: Response) -> Self {
        stream::once(future::ready(response)).boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_append_errors_path_fallback_and_override() {
        let expected_errors = vec![
            Error {
                message: "Something terrible happened!".to_string(),
                path: Some(Path::from("here")),
                ..Default::default()
            },
            Error {
                message: "I mean for real".to_string(),
                ..Default::default()
            },
        ];

        let mut errors_to_append = vec![
            Error {
                message: "Something terrible happened!".to_string(),
                path: Some(Path::from("here")),
                ..Default::default()
            },
            Error {
                message: "I mean for real".to_string(),
                ..Default::default()
            },
        ];

        let mut response = Response::builder().build();
        response.append_errors(&mut errors_to_append);
        assert_eq!(response.errors, expected_errors);
    }

    #[test]
    fn test_response() {
        let result = serde_json::from_str::<Response>(
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
            Response::builder()
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
                .errors(vec![Error {
                    message: "Name for character with ID 1002 could not be fetched.".into(),
                    locations: vec!(Location { line: 6, column: 7 }),
                    path: Some(Path::from("hero/heroFriends/1/name")),
                    extensions: bjson!({
                        "error-extension": 5,
                    })
                    .as_object()
                    .cloned()
                    .unwrap()
                }])
                .extensions(
                    bjson!({
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
        let result = serde_json::from_str::<Response>(
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
            Response::builder()
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
                .errors(vec![Error {
                    message: "Name for character with ID 1002 could not be fetched.".into(),
                    locations: vec!(Location { line: 6, column: 7 }),
                    path: Some(Path::from("hero/heroFriends/1/name")),
                    extensions: bjson!({
                        "error-extension": 5,
                    })
                    .as_object()
                    .cloned()
                    .unwrap()
                }])
                .extensions(
                    bjson!({
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
