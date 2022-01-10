use crate::prelude::graphql::*;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

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

/// temporary structure to help deserializing errors
#[derive(Deserialize)]
struct ResponseMeta {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    label: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    path: Option<Path>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    has_next: Option<bool>,
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

    pub fn from_bytes(service_name: &str, b: Bytes) -> Result<Response, FetchError> {
        let mut value =
            Value::from_bytes(b).map_err(|error| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: error.to_string(),
            })?;

        let (data, errors, extensions) = match &mut value {
            Value::Object(object) => (
                object.remove("data").unwrap_or_default(),
                match object.remove("errors") {
                    Some(Value::Array(v)) => {
                        let res: Result<Vec<Error>, FetchError> = v
                            .into_iter()
                            .map(|v| Error::from_value(service_name, v))
                            .collect();
                        res?
                    }
                    None => Vec::new(),
                    _ => {
                        return Err(FetchError::SubrequestMalformedResponse {
                            service: service_name.to_string(),
                            reason: "expected a JSON array".to_string(),
                        })
                    }
                },
                match object.remove("extensions") {
                    Some(Value::Object(o)) => o,
                    None => Object::default(),
                    _ => {
                        return Err(FetchError::SubrequestMalformedResponse {
                            service: service_name.to_string(),
                            reason: "expected a JSON object".to_string(),
                        })
                    }
                },
            ),
            _ => {
                return Err(FetchError::SubrequestMalformedResponse {
                    service: service_name.to_string(),
                    reason: "expected a JSON object".to_string(),
                })
            }
        };

        let ResponseMeta {
            label,
            path,
            has_next,
        } = serde_json_bytes::from_value(value).map_err(|error| {
            FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: error.to_string(),
            }
        })?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use serde_json_bytes::bjson;

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
