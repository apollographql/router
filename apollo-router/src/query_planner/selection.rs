use apollo_compiler::ast::Name;
use apollo_compiler::schema::ExtendedType;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Entry;

use crate::json_ext::Object;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::spec::Schema;
use crate::spec::TYPENAME;

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
    pub(crate) alias: Option<Name>,

    /// The name of the field.
    pub(crate) name: Name,

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
    pub(crate) type_condition: Option<Name>,

    /// The selections from the fragment.
    pub(crate) selections: Vec<Selection>,
}

pub(crate) fn execute_selection_set<'a>(
    input_content: &'a Value,
    selections: &[Selection],
    schema: &Schema,
    mut current_type: Option<&'a str>,
) -> Value {
    let content = match input_content.as_object() {
        Some(o) => o,
        None => return Value::Null,
    };

    current_type = content
        .get(TYPENAME)
        .and_then(|v| v.as_str())
        .or(current_type);

    let mut output = Object::with_capacity(selections.len());
    for selection in selections {
        match selection {
            Selection::Field(Field {
                alias,
                name,
                selections,
            }) => {
                let selection_name = alias.as_ref().map(|a| a.as_str()).unwrap_or(name.as_str());
                let field_type = current_type.and_then(|t| {
                    schema
                        .supergraph_schema()
                        .types
                        .get(t)
                        .and_then(|ty| match ty {
                            apollo_compiler::schema::ExtendedType::Object(o) => {
                                o.fields.get(name.as_str()).map(|f| &f.ty)
                            }
                            apollo_compiler::schema::ExtendedType::Interface(i) => {
                                i.fields.get(name.as_str()).map(|f| &f.ty)
                            }
                            _ => None,
                        })
                });

                match content.get_key_value(selection_name) {
                    None => {
                        if name == TYPENAME {
                            // if the __typename field was missing but we can infer it, fill it
                            if let Some(ty) = current_type {
                                output.insert(
                                    ByteString::from(selection_name.to_owned()),
                                    Value::String(ByteString::from(ty.to_owned())),
                                );
                                continue;
                            }
                        }
                        // the behaviour here does not align with the gateway: we should instead assume that
                        // data is in the correct shape, and return a null (or even no value at all) on
                        // missing fields. If a field was missing, it should have been nullified,
                        // and if it was non nullable, the parent object would have been nullified.
                        // Unfortunately, we don't validate subgraph responses yet
                        if field_type
                            .as_ref()
                            .map(|ty| !ty.is_non_null())
                            .unwrap_or(false)
                        {
                            output.insert(ByteString::from(selection_name.to_owned()), Value::Null);
                        } else {
                            return Value::Null;
                        }
                    }
                    Some((key, value)) => {
                        if let Some(elements) = value.as_array() {
                            let selected = elements
                                .iter()
                                .map(|element| match selections {
                                    Some(sels) => execute_selection_set(
                                        element,
                                        sels,
                                        schema,
                                        field_type
                                            .as_ref()
                                            .map(|ty| ty.inner_named_type().as_str()),
                                    ),
                                    None => element.clone(),
                                })
                                .collect::<Vec<_>>();
                            output.insert(key.clone(), Value::Array(selected));
                        } else if let Some(sels) = selections {
                            output.insert(
                                key.clone(),
                                execute_selection_set(
                                    value,
                                    sels,
                                    schema,
                                    field_type.as_ref().map(|ty| ty.inner_named_type().as_str()),
                                ),
                            );
                        } else {
                            output.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
            Selection::InlineFragment(InlineFragment {
                type_condition,
                selections,
            }) => match type_condition {
                None => continue,
                Some(condition) => {
                    if type_condition_matches(schema, current_type, condition) {
                        if let Value::Object(selected) =
                            execute_selection_set(input_content, selections, schema, current_type)
                        {
                            for (key, value) in selected.into_iter() {
                                match output.entry(key) {
                                    Entry::Vacant(e) => {
                                        e.insert(value);
                                    }
                                    Entry::Occupied(e) => {
                                        e.into_mut().deep_merge(value);
                                    }
                                }
                            }
                        }
                    }
                }
            },
        }
    }

    Value::Object(output)
}

/// This is similar to DoesFragmentTypeApply from the GraphQL spec, but the
/// `current_type` could be an abstract type or None (we're not yet implementing
/// CompleteValue and ResolveAbstractType). So this function is more flexible,
/// checking if the condition is a subtype of the current type, or vice versa.
///
/// <https://spec.graphql.org/October2021/#DoesFragmentTypeApply()>
/// <https://spec.graphql.org/October2021/#CompleteValue()>
/// <https://spec.graphql.org/October2021/#ResolveAbstractType()>
fn type_condition_matches(
    schema: &Schema,
    current_type: Option<&str>,
    type_condition: &str,
) -> bool {
    // Not having a current type is probably invalid, but this is not the place to check it.
    let current_type = match current_type {
        Some(t) => t,
        None => return false,
    };

    if current_type == type_condition {
        return true;
    }

    let current_type = match schema.supergraph_schema().types.get(current_type) {
        None => return false,
        Some(t) => t,
    };

    let conditional_type = match schema.supergraph_schema().types.get(type_condition) {
        None => return false,
        Some(t) => t,
    };

    use ExtendedType::*;
    match current_type {
        Object(object_type) => match conditional_type {
            Interface(interface_type) => object_type
                .implements_interfaces
                .contains(&interface_type.name),

            Union(union_type) => union_type.members.contains(&object_type.name),

            _ => false,
        },

        Interface(interface_type) => match conditional_type {
            Interface(conditional_type) => conditional_type
                .implements_interfaces
                .contains(&interface_type.name),

            Object(object_type) => object_type
                .implements_interfaces
                .contains(&interface_type.name),

            _ => false,
        },

        Union(union_type) => match conditional_type {
            Object(object_type) => union_type.members.contains(&object_type.name),

            _ => false,
        },

        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use serde_json_bytes::json as bjson;

    use super::Selection;
    use super::*;
    use crate::error::FetchError;
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
                .map(|value| execute_selection_set(value, selections, schema, None))
                .collect::<Vec<_>>(),
        ))
    }

    macro_rules! select {
        ($schema:expr, $content:expr $(,)?) => {{
            let schema = Schema::parse_test(&$schema, &Default::default()).unwrap();
            let response = Response::builder()
                .data($content)
                .build();
            // equivalent to "... on OtherStuffToIgnore {} ... on User { __typename id job { name } }"
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
        // equivalent to "... on OtherStuffToIgnore {} ... on User { __typename id job { name } }"

        assert_eq!(
            select!(
                include_str!("testdata/schema.graphql"),
                json!({"__typename": "User", "name":"Bob", "job":{"name":"astronaut"}}),
            )
            .unwrap(),
            bjson!([{}])
        );
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

        let value = execute_selection_set(&response, &selection, &schema, None);
        println!(
            "response\n{}\nand selection\n{:?}\n returns:\n{}",
            serde_json::to_string_pretty(&response).unwrap(),
            selection,
            serde_json::to_string_pretty(&value).unwrap()
        );

        assert_eq!(
            value,
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

    #[test]
    fn test_execute_selection_set_abstract_types() {
        let schema = with_supergraph_boilerplate(
            "type Query { hello: String }
            type Entity {
              id: Int!
              nestedUnion: NestedUnion
              nestedInterface: WrapperNestedInterface
              objectWithinUnion: Union1
              objectWithinInterface: NestedObject
            }
            union NestedUnion = Union1 | Union2
            type Union1 {
              id: Int!
              field: Int!
            }
            type Union2 {
              id: Int!
            }
            interface WrapperNestedInterface {
              id: Int!
            }
            interface NestedInterface implements WrapperNestedInterface {
              id: Int!
            }
            type NestedObject implements NestedInterface & WrapperNestedInterface {
              id: Int!
              field: Int!
            }
            type NestedObject2 implements NestedInterface & WrapperNestedInterface {
              id: Int!
            }",
        );
        let schema = Schema::parse_test(&schema, &Default::default()).unwrap();

        let response = bjson!({
          "__typename": "Entity",
          "id": 1780384,
          "nestedUnion": {
            "__typename": "Union1",
            "id": 1780384,
            "field": 1,
          },
          "nestedInterface": {
            "__typename": "NestedObject",
            "id": 1780384,
            "field": 1,
          },
          "objectWithinUnion": {
            "__typename": "Union2",
            "id": 1780384,
          },
          "objectWithinInterface": {
            "__typename": "NestedObject2",
            "id": 1780384,
          },
        });

        let requires = json!([
          {
            "kind": "InlineFragment",
            "typeCondition": "Entity",
            "selections": [
              {
                "kind": "Field",
                "name": "__typename"
              },
              {
                "kind": "Field",
                "name": "nestedUnion",
                "selections": [
                  {
                    "kind": "InlineFragment",
                    "typeCondition": "Union1",
                    "selections": [
                      {
                        "kind": "Field",
                        "name": "__typename"
                      },
                      {
                        "kind": "Field",
                        "name": "id"
                      },
                      {
                        "kind": "Field",
                        "name": "field"
                      },
                    ]
                  },
                  {
                    "kind": "InlineFragment",
                    "typeCondition": "Union2",
                    "selections": [
                      {
                        "kind": "Field",
                        "name": "__typename"
                      },
                      {
                        "kind": "Field",
                        "name": "id"
                      },
                    ]
                  }
                ]
              },
              {
                "kind": "Field",
                "name": "nestedInterface",
                "selections": [
                  {
                    "kind": "InlineFragment",
                    "typeCondition": "NestedObject",
                    "selections": [
                      {
                        "kind": "Field",
                        "name": "__typename"
                      },
                      {
                        "kind": "Field",
                        "name": "id"
                      },
                      {
                        "kind": "Field",
                        "name": "field"
                      },
                    ]
                  },
                  {
                    "kind": "InlineFragment",
                    "typeCondition": "NestedObject2",
                    "selections": [
                      {
                        "kind": "Field",
                        "name": "__typename"
                      },
                      {
                        "kind": "Field",
                        "name": "id"
                      },
                    ]
                  }
                ]
              },
              {
                "kind": "Field",
                "name": "objectWithinUnion",
                "selections": [
                  {
                    "kind": "InlineFragment",
                    "typeCondition": "NestedUnion",
                    "selections": [
                      {
                        "kind": "InlineFragment",
                        "typeCondition": "Union1",
                        "selections": [
                          {
                            "kind": "Field",
                            "name": "__typename"
                          },
                          {
                            "kind": "Field",
                            "name": "id"
                          },
                          {
                            "kind": "Field",
                            "name": "field"
                          },
                        ]
                      },
                      {
                        "kind": "InlineFragment",
                        "typeCondition": "Union2",
                        "selections": [
                          {
                            "kind": "Field",
                            "name": "__typename"
                          },
                          {
                            "kind": "Field",
                            "name": "id"
                          },
                        ]
                      }
                    ]
                  },
                ]
              },
              {
                "kind": "Field",
                "name": "objectWithinInterface",
                "selections": [
                  {
                    "kind": "InlineFragment",
                    "typeCondition": "NestedInterface",
                    "selections": [
                      {
                        "kind": "InlineFragment",
                        "typeCondition": "NestedObject",
                        "selections": [
                          {
                            "kind": "Field",
                            "name": "__typename"
                          },
                          {
                            "kind": "Field",
                            "name": "id"
                          },
                          {
                            "kind": "Field",
                            "name": "field"
                          },
                        ]
                      },
                      {
                        "kind": "InlineFragment",
                        "typeCondition": "NestedObject2",
                        "selections": [
                          {
                            "kind": "Field",
                            "name": "__typename"
                          },
                          {
                            "kind": "Field",
                            "name": "id"
                          },
                        ]
                      }
                    ]
                  },
                ]
              },
              {
                "kind": "Field",
                "name": "id"
              },
            ]
          }
        ]);

        let selection: Vec<Selection> = serde_json::from_value(requires).unwrap();

        let value = execute_selection_set(&response, &selection, &schema, None);

        assert_eq!(
            value,
            bjson!({
                "__typename": "Entity",
                "nestedUnion": {
                    "__typename": "Union1",
                    "id": 1780384,
                    "field": 1,
                },
                "nestedInterface": {
                  "__typename": "NestedObject",
                  "id": 1780384,
                  "field": 1,
                },
                "objectWithinUnion": {
                  "__typename": "Union2",
                  "id": 1780384,
                },
                "objectWithinInterface": {
                  "__typename": "NestedObject2",
                  "id": 1780384,
                },
                "id": 1780384,
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
