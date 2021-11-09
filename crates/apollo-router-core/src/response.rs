use crate::prelude::graphql::*;
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

    pub fn select(
        &self,
        path: &Path,
        selections: &[Selection],
        schema: &Schema,
    ) -> Result<Value, FetchError> {
        let values =
            self.data
                .get_at_path(path)
                .map_err(|err| FetchError::ExecutionPathNotFound {
                    reason: err.to_string(),
                })?;

        Ok(Value::Array(
            values
                .into_iter()
                .flat_map(|value| match (value, selections) {
                    (Value::Object(content), requires) => {
                        select_object(content, requires, schema).transpose()
                    }
                    (_, _) => Some(Err(FetchError::ExecutionInvalidContent {
                        reason: "not an object".to_string(),
                    })),
                })
                .collect::<Result<Vec<_>, _>>()?,
        ))
    }

    pub fn insert_data(&mut self, path: &Path, value: Value) -> Result<(), FetchError> {
        let nodes =
            self.data
                .get_at_path_mut(path)
                .map_err(|err| FetchError::ExecutionPathNotFound {
                    reason: err.to_string(),
                })?;

        for node in nodes {
            node.deep_merge(&value);
        }

        Ok(())
    }

    /// append_errors default the errors `path` with the one provided.
    pub fn append_errors(&mut self, errors: &mut Vec<Error>) {
        self.errors.append(errors)
    }
}

fn select_object(
    content: &Object,
    selections: &[Selection],
    schema: &Schema,
) -> Result<Option<Value>, FetchError> {
    let mut output = Object::new();
    for selection in selections {
        match selection {
            Selection::Field(field) => {
                if let Some(value) = select_field(content, field, schema)? {
                    output
                        .entry(field.name.to_owned())
                        .and_modify(|existing| existing.deep_merge(&value))
                        .or_insert(value);
                }
            }
            Selection::InlineFragment(fragment) => {
                if let Some(Value::Object(value)) =
                    select_inline_fragment(content, fragment, schema)?
                {
                    output.append(&mut value.to_owned())
                }
            }
            Selection::FragmentSpread(_fragment) => {
                unimplemented!("do we get in a case where it is used in the router?")
            }
        };
    }
    if output.is_empty() {
        return Ok(None);
    }
    Ok(Some(Value::Object(output)))
}

fn select_field(
    content: &Object,
    field: &Field,
    schema: &Schema,
) -> Result<Option<Value>, FetchError> {
    match (content.get(&field.name), &field.selections) {
        (Some(Value::Object(child)), Some(selections)) => select_object(child, selections, schema),
        (Some(value), None) => Ok(Some(value.to_owned())),
        (None, _) => Err(FetchError::ExecutionFieldNotFound {
            field: field.name.to_owned(),
        }),
        _ => Ok(None),
    }
}

fn select_inline_fragment(
    content: &Object,
    fragment: &InlineFragment,
    schema: &Schema,
) -> Result<Option<Value>, FetchError> {
    match (&fragment.type_condition, &content.get("__typename")) {
        (Some(condition), Some(Value::String(typename))) => {
            if condition == typename || schema.is_subtype(condition, typename) {
                select_object(content, &fragment.selections, schema)
            } else {
                Ok(None)
            }
        }
        (None, _) => select_object(content, &fragment.selections, schema),
        (_, None) => Err(FetchError::ExecutionFieldNotFound {
            field: "__typename".to_string(),
        }),
        (_, _) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    macro_rules! select {
        ($schema:expr, $content:expr $(,)?) => {{
            let schema: Schema = $schema.parse().unwrap();
            let response = Response::builder()
                .data($content)
                .build();
            let stub = json!([
                {
                    "kind": "InlineFragment",
                    "typeCondition": "OtherStuffToIgnore",
                    "selections": [],
                },
                {
                    "kind": "InlineFragment",
                    "typeCondition": "User",
                    "selections": [
                        {
                            "kind": "Field",
                            "name": "__typename",
                        },
                        {
                            "kind": "Field",
                            "name": "id",
                        },
                        {
                            "kind": "Field",
                            "name": "job",
                            "selections": [
                                {
                                    "kind": "Field",
                                    "name": "name",
                                }
                            ],
                        }
                      ]
                },
            ]);
            let selection: Vec<Selection> = serde_json::from_value(stub).unwrap();
            response.select(&Path::empty(), &selection, &schema)
        }};
    }

    #[test]
    fn test_selection() {
        assert_eq!(
            select!(
                "",
                json!({"__typename": "User", "id":2, "name":"Bob", "job":{"name":"astronaut"}}),
            )
            .unwrap(),
            json!([{
                "__typename": "User",
                "id": 2,
                "job": {
                    "name": "astronaut"
                }
            }]),
        );
    }

    #[test]
    fn test_selection_subtype() {
        assert_eq!(
            select!(
                "union User = Author | Reviewer",
                json!({"__typename": "Author", "id":2, "name":"Bob", "job":{"name":"astronaut"}}),
            )
            .unwrap(),
            json!([{
                "__typename": "Author",
                "id": 2,
                "job": {
                    "name": "astronaut"
                }
            }]),
        );
    }

    #[test]
    fn test_selection_missing_field() {
        assert!(matches!(
            select!(
                "",
                json!({"__typename": "User", "name":"Bob", "job":{"name":"astronaut"}}),
            )
                .unwrap_err(),
            FetchError::ExecutionFieldNotFound { field } if field == "id"
        ));
    }

    #[test]
    fn test_insert_data() {
        let mut response = Response::builder()
            .data(json!({
                "name": "SpongeBob",
                "job": {},
            }))
            .build();
        response
            .insert_data(
                &Path::from("job"),
                json!({
                    "name": "cook",
                }),
            )
            .unwrap();
        assert_eq!(
            response.data,
            json!({
                "name": "SpongeBob",
                "job": {
                    "name": "cook",
                },
            }),
        );
    }

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
