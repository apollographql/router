#![allow(missing_docs)] // FIXME
use std::time::Instant;

use bytes::Bytes;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;

use crate::error::Error;
use crate::error::FetchError;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::Value;

/// A graphql primary response.
/// Used for federated and subgraph queries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Response {
    /// The label that was passed to the defer or stream directive for this patch.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub label: Option<String>,

    /// The response data.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub data: Option<Value>,

    /// The path that the data should be merged at.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<Path>,

    /// The optional graphql errors encountered.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub errors: Vec<Error>,

    /// The optional graphql extensions.
    #[serde(skip_serializing_if = "Object::is_empty", default)]
    pub extensions: Object,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub has_next: Option<bool>,

    #[serde(skip, default)]
    pub subscribed: Option<bool>,

    /// Used for subscription event to compute the duration of a subscription event
    #[serde(skip, default)]
    pub created_at: Option<Instant>,

    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub incremental: Vec<IncrementalResponse>,
}

#[buildstructor::buildstructor]
impl Response {
    /// Constructor
    #[builder(visibility = "pub")]
    #[allow(clippy::too_many_arguments)]
    fn new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
        extensions: Map<ByteString, Value>,
        _subselection: Option<String>,
        has_next: Option<bool>,
        subscribed: Option<bool>,
        incremental: Vec<IncrementalResponse>,
        created_at: Option<Instant>,
    ) -> Self {
        Self {
            label,
            data,
            path,
            errors,
            extensions,
            has_next,
            subscribed,
            incremental,
            created_at,
        }
    }

    /// If path is None, this is a primary response.
    pub fn is_primary(&self) -> bool {
        self.path.is_none()
    }

    /// append_errors default the errors `path` with the one provided.
    pub fn append_errors(&mut self, errors: &mut Vec<Error>) {
        self.errors.append(errors)
    }

    /// Create a [`Response`] from the supplied [`Bytes`].
    ///
    /// This will return an error (identifying the faulty service) if the input is invalid.
    pub(crate) fn from_bytes(service_name: &str, b: Bytes) -> Result<Response, FetchError> {
        let value =
            Value::from_bytes(b).map_err(|error| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: error.to_string(),
            })?;
        let mut object =
            ensure_object!(value).map_err(|error| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: error.to_string(),
            })?;

        let data = object.remove("data");
        let errors = extract_key_value_from_object!(object, "errors", Value::Array(v) => v)
            .map_err(|err| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: err.to_string(),
            })?
            .into_iter()
            .flatten()
            .map(|v| Error::from_value(service_name, v))
            .collect::<Result<Vec<Error>, FetchError>>()?;
        let extensions =
            extract_key_value_from_object!(object, "extensions", Value::Object(o) => o)
                .map_err(|err| FetchError::SubrequestMalformedResponse {
                    service: service_name.to_string(),
                    reason: err.to_string(),
                })?
                .unwrap_or_default();
        let label = extract_key_value_from_object!(object, "label", Value::String(s) => s)
            .map_err(|err| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: err.to_string(),
            })?
            .map(|s| s.as_str().to_string());
        let path = extract_key_value_from_object!(object, "path")
            .map(serde_json_bytes::from_value)
            .transpose()
            .map_err(|err| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: err.to_string(),
            })?;
        let has_next = extract_key_value_from_object!(object, "hasNext", Value::Bool(b) => b)
            .map_err(|err| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: err.to_string(),
            })?;
        let incremental =
            extract_key_value_from_object!(object, "incremental", Value::Array(a) => a).map_err(
                |err| FetchError::SubrequestMalformedResponse {
                    service: service_name.to_string(),
                    reason: err.to_string(),
                },
            )?;
        let incremental: Vec<IncrementalResponse> = match incremental {
            Some(v) => v
                .into_iter()
                .map(serde_json_bytes::from_value)
                .collect::<Result<Vec<IncrementalResponse>, _>>()
                .map_err(|err| FetchError::SubrequestMalformedResponse {
                    service: service_name.to_string(),
                    reason: err.to_string(),
                })?,
            None => vec![],
        };
        // Graphql spec says:
        // If the data entry in the response is not present, the errors entry in the response must not be empty.
        // It must contain at least one error. The errors it contains should indicate why no data was able to be returned.
        if data.is_none() && errors.is_empty() {
            return Err(FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: "graphql response without data must contain at least one error".to_string(),
            });
        }

        Ok(Response {
            label,
            data,
            path,
            errors,
            extensions,
            has_next,
            subscribed: None,
            incremental,
            created_at: None,
        })
    }
}

/// A graphql incremental response.
/// Used with `@defer`
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct IncrementalResponse {
    /// The label that was passed to the defer or stream directive for this patch.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub label: Option<String>,

    /// The response data.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub data: Option<Value>,

    /// The path that the data should be merged at.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<Path>,

    /// The optional graphql errors encountered.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub errors: Vec<Error>,

    /// The optional graphql extensions.
    #[serde(skip_serializing_if = "Object::is_empty", default)]
    pub extensions: Object,
}

#[buildstructor::buildstructor]
impl IncrementalResponse {
    /// Constructor
    #[builder(visibility = "pub")]
    fn new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
        extensions: Map<ByteString, Value>,
    ) -> Self {
        Self {
            label,
            data,
            path,
            errors,
            extensions,
        }
    }

    /// append_errors default the errors `path` with the one provided.
    pub fn append_errors(&mut self, errors: &mut Vec<Error>) {
        self.errors.append(errors)
    }
}

#[cfg(test)]
mod tests {
    use router_bridge::planner::Location;
    use serde_json::json;
    use serde_json_bytes::json as bjson;

    use super::*;

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
                .has_next(true)
                .build()
        );
    }

    #[test]
    fn test_no_data_and_no_errors() {
        let response = Response::from_bytes("test", "{\"errors\":null}".into());
        assert_eq!(
            response.expect_err("no data and no errors"),
            FetchError::SubrequestMalformedResponse {
                service: "test".to_string(),
                reason: "graphql response without data must contain at least one error".to_string(),
            }
        );
    }
}
