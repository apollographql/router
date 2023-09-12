use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;

use crate::error::FetchError;
use crate::json_ext::Object;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::spec::Schema;

/// A selection that is part of a fetch.
/// Selections are used to propagate data to subgraph fetches.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase", tag = "kind")]
pub(crate) enum Selection {
    /// A field selection.
    Field(Field),

    /// An inline fragment selection.
    InlineFragment(InlineFragment),
}

/// The field that is used
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Field {
    /// An optional alias for the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) alias: Option<String>,

    /// The name of the field.
    pub(crate) name: String,

    /// The selections for the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) selections: Option<Vec<Selection>>,
}

/// An inline fragment.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InlineFragment {
    /// The required fragment type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) type_condition: Option<String>,

    /// The selections from the fragment.
    pub(crate) selections: Vec<Selection>,
}

pub(crate) fn select_object(
    content: &Object,
    selections: &[Selection],
    schema: &Schema,
) -> Result<Option<Value>, FetchError> {
    let mut output = Object::with_capacity(selections.len());
    for selection in selections {
        match selection {
            Selection::Field(field) => {
                if let Some((key, value)) = select_field(content, field, schema)? {
                    if let Some(o) = output.get_mut(field.name.as_str()) {
                        o.deep_merge(value);
                    } else {
                        output.insert(key.to_owned(), value);
                    }
                }
            }
            Selection::InlineFragment(fragment) => {
                if let Some(Value::Object(value)) =
                    select_inline_fragment(content, fragment, schema)?
                {
                    output.append(&mut value.to_owned())
                }
            }
        };
    }
    if output.is_empty() {
        return Ok(None);
    }
    Ok(Some(Value::Object(output)))
}

fn select_field<'a>(
    content: &'a Object,
    field: &Field,
    schema: &Schema,
) -> Result<Option<(&'a ByteString, Value)>, FetchError> {
    let res = match (
        content.get_key_value(field.name.as_str()),
        &field.selections,
    ) {
        (Some((k, v)), _) => select_value(v, field, schema).map(|opt| opt.map(|v| (k, v))),
        (None, _) => Err(FetchError::ExecutionFieldNotFound {
            field: field.name.to_owned(),
        }),
    };
    res
}

fn select_inline_fragment(
    content: &Object,
    fragment: &InlineFragment,
    schema: &Schema,
) -> Result<Option<Value>, FetchError> {
    match (&fragment.type_condition, &content.get("__typename")) {
        (Some(condition), Some(Value::String(typename))) => {
            if condition == typename || schema.is_subtype(condition, typename.as_str()) {
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

fn select_value(
    content: &Value,
    field: &Field,
    schema: &Schema,
) -> Result<Option<Value>, FetchError> {
    match (content, &field.selections) {
        (Value::Object(child), Some(selections)) => select_object(child, selections, schema),
        (Value::Array(elements), Some(_)) => elements
            .iter()
            .map(|element| select_value(element, field, schema))
            .collect(),
        (value, None) => Ok(Some(value.to_owned())),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use serde_json_bytes::json as bjson;

    use super::Selection;
    use super::*;
    use crate::graphql::Response;
    use crate::json_ext::Path;

    fn select(
        response: &Response,
        path: &Path,
        selections: &[Selection],
        schema: &Schema,
    ) -> Result<Value, FetchError> {
        let mut values = Vec::new();
        response
            .data
            .as_ref()
            .unwrap()
            .select_values_and_paths(schema, path, |_path, value| {
                values.push(value);
            });

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

    macro_rules! select {
        ($schema:expr, $content:expr $(,)?) => {{
            let schema = Schema::parse_test(&$schema, &Default::default()).unwrap();
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
            select(&response, &Path::empty(), &selection, &schema)
        }};
    }

    #[test]
    fn test_selection() {
        assert_eq!(
            select!(
                include_str!("testdata/schema.graphql"),
                bjson!({"__typename": "User", "id":2, "name":"Bob", "job":{"name":"astronaut"}}),
            )
            .unwrap(),
            bjson!([{
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
                with_supergraph_boilerplate(
                    "type Query { me: String } type Author { name: String } type Reviewer { name: String } \
                    union User = Author | Reviewer"
                ),
                bjson!({"__typename": "Author", "id":2, "name":"Bob", "job":{"name":"astronaut"}}),
            )
            .unwrap(),
            bjson!([{
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
                include_str!("testdata/schema.graphql"),
                json!({"__typename": "User", "name":"Bob", "job":{"name":"astronaut"}}),
            )
                .unwrap_err(),
            FetchError::ExecutionFieldNotFound { field } if field == "id"
        ));
    }

    #[test]
    fn test_array() {
        let schema = with_supergraph_boilerplate(
            "type Query { me: String }
            type MainObject { mainObjectList: [SubObject] }
            type SubObject { key: String name: String }",
        );
        let schema = Schema::parse_test(&schema, &Default::default()).unwrap();

        let response = bjson!({
            "__typename": "MainObject",
            "mainObjectList": [
                {
                    "key": "a",
                    "name": "A"
                },
                {
                    "key": "b",
                    "name": "B"
                }
            ]
        });

        let requires = json!([
            {
                "kind": "InlineFragment",
                "typeCondition": "MainObject",
                "selections": [
                    {
                        "kind": "Field",
                        "name": "__typename",
                    },
                    {
                        "kind": "Field",
                        "name": "mainObjectList",
                        "selections": [
                            {
                                "kind": "Field",
                                "name": "key",
                            }
                        ],
                    }
                ],
            },
        ]);
        let selection: Vec<Selection> = serde_json::from_value(requires).unwrap();

        let value = select_object(response.as_object().unwrap(), &selection, &schema);
        println!(
            "response\n{}\nand selection\n{:?}\n returns:\n{}",
            serde_json::to_string_pretty(&response).unwrap(),
            selection,
            serde_json::to_string_pretty(&value).unwrap()
        );

        assert_eq!(
            value.unwrap().unwrap(),
            bjson!({
                "__typename": "MainObject",
                "mainObjectList": [
                    {
                        "key": "a"
                    },
                    {
                        "key": "b"
                    }
                ]
            })
        );
    }

    fn with_supergraph_boilerplate(content: &str) -> String {
        format!(
            "{}\n{}",
            r#"
        schema
            @core(feature: "https://specs.apollo.dev/core/v0.1")
            @core(feature: "https://specs.apollo.dev/join/v0.1") {
            query: Query
        }
        directive @core(feature: String!) repeatable on SCHEMA
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        enum join__Graph {
            TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
        }

        "#,
            content
        )
    }
}
