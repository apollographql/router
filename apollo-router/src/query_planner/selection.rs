use apollo_compiler::schema::ExtendedType;
use apollo_federation::query_plan::requires_selection::Field;
use apollo_federation::query_plan::requires_selection::InlineFragment;
use apollo_federation::query_plan::requires_selection::Selection;
use apollo_json::JsonKind;
use apollo_json::NewValue;
use apollo_json::PendingId;
use apollo_json::PendingTree;

#[cfg(test)]
use crate::json_ext;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::spec::Schema;
use crate::spec::TYPENAME;

/// Merges the member at `incoming` into the one at `existing`, for a key that
/// two inline fragments both select. Both are sealed first, so the merge is
/// the one sealed values get.
fn merge_member(
    tree: &mut PendingTree<'_>,
    existing: PendingId,
    incoming: PendingId,
    schema: &Schema,
) {
    let mut merged = tree.take(existing);
    merged.type_aware_deep_merge(tree.take(incoming), schema);
    tree.set(existing, NewValue::Node(merged));
}

#[cfg(test)]
pub(crate) fn execute_selection_set(
    input_content: &Value,
    selections: &[Selection],
    schema: &Schema,
    current_type: Option<&str>,
) -> Value {
    let mut tree = PendingTree::new();
    let root = tree.root();
    select(
        &mut tree,
        root,
        input_content,
        selections,
        schema,
        current_type,
    );
    tree.seal(root)
}

/// Selects `selections` from `input_content` into the slot `output` of a
/// pending tree, so a caller building many selections writes them all into
/// one arena without a buffer per object. Keys borrow from `selections`, and
/// selected leaves are adopted by reference. `output` ends up holding an
/// object, or `null` when the input is not an object or lacks a non-null
/// field.
pub(crate) fn select<'a>(
    tree: &mut PendingTree<'a>,
    output: PendingId,
    input_content: &Value,
    selections: &'a [Selection],
    schema: &Schema,
    current_type: Option<&str>,
) {
    if input_content.kind() != JsonKind::Object {
        tree.set(output, NewValue::Null);
        return;
    }

    let typename = input_content.get(TYPENAME);
    let own_type = typename.as_ref().and_then(|v| v.as_str());
    let current_type = own_type.as_deref().or(current_type);

    tree.set_object(output);
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

                match input_content.get(selection_name) {
                    None => {
                        if name == TYPENAME {
                            // if the __typename field was missing but we can infer it, fill it
                            if let Some(ty) = current_type {
                                let member = tree.member_slot(output, selection_name);
                                tree.set(member, ty.to_owned());
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
                            let member = tree.member_slot(output, selection_name);
                            tree.set(member, NewValue::Null);
                        } else {
                            tree.set(output, NewValue::Null);
                            return;
                        }
                    }
                    Some(value) => {
                        let member = tree.member_slot(output, selection_name);
                        if selections.is_empty() {
                            tree.set(member, NewValue::Node(value));
                        } else {
                            let inner_type =
                                field_type.as_ref().map(|ty| ty.inner_named_type().as_str());
                            if value.kind() == JsonKind::Array {
                                tree.set_array(member);
                                for element in value.array_iter() {
                                    let slot = tree.push(member);
                                    select(tree, slot, &element, selections, schema, inner_type);
                                }
                            } else {
                                select(tree, member, &value, selections, schema, inner_type);
                            }
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
                    if !type_condition_matches(schema, current_type, condition) {
                        continue;
                    }
                    let selected = tree.root();
                    select(
                        tree,
                        selected,
                        input_content,
                        selections,
                        schema,
                        current_type,
                    );
                    if !tree.is_object(selected) {
                        continue;
                    }
                    tree.merge_members(output, selected, |tree, existing, incoming| {
                        merge_member(tree, existing, incoming, schema)
                    });
                }
            },
        }
    }
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
    use apollo_json::json as bjson;
    use serde_json::json;

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

        Ok(json_ext::array(values.iter().map(|value| {
            execute_selection_set(value, selections, schema, None)
        })))
    }

    macro_rules! select {
        ($schema:expr, $content:expr $(,)?) => {{
            let schema = Schema::parse(&$schema, &Default::default()).unwrap();
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
                    "type Query @join__type(graph: TEST) { me: String @join__field(graph: TEST) } type Author { name: String } type Reviewer { name: String } \
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
                bjson!({"__typename": "User", "name":"Bob", "job":{"name":"astronaut"}}),
            )
            .unwrap(),
            bjson!([{}])
        );
    }

    #[test]
    fn test_array() {
        let schema = with_supergraph_boilerplate(
            "type Query @join__type(graph: TEST){ me: String @join__field(graph: TEST) }
            type MainObject { mainObjectList: [SubObject] }
            type SubObject { key: String name: String }",
        );
        let schema = Schema::parse(&schema, &Default::default()).unwrap();

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
    fn fragments_selecting_the_same_field_merge_it_in_first_position() {
        let schema = with_supergraph_boilerplate(
            "type Query @join__type(graph: TEST){ me: String @join__field(graph: TEST) }
            type MainObject { mainObjectList: [SubObject] other: String }
            type SubObject { key: String name: String }",
        );
        let schema = Schema::parse(&schema, &Default::default()).unwrap();
        let response = bjson!({
            "__typename": "MainObject",
            "other": "o",
            "mainObjectList": [{"key": "a", "name": "A"}, {"key": "b", "name": "B"}]
        });
        let list_of = |field: &str| {
            json!({
                "kind": "Field",
                "name": "mainObjectList",
                "selections": [{"kind": "Field", "name": field}],
            })
        };
        let requires = json!([
            {
                "kind": "InlineFragment",
                "typeCondition": "MainObject",
                "selections": [list_of("key")],
            },
            {"kind": "Field", "name": "other"},
            {
                "kind": "InlineFragment",
                "typeCondition": "MainObject",
                "selections": [{"kind": "Field", "name": "__typename"}, list_of("name")],
            },
        ]);
        let selection: Vec<Selection> = serde_json::from_value(requires).unwrap();

        let value = execute_selection_set(&response, &selection, &schema, None);

        assert_eq!(
            value.to_string(),
            r#"{"mainObjectList":[{"key":"a","name":"A"},{"key":"b","name":"B"}],"other":"o","__typename":"MainObject"}"#
        );
    }

    #[test]
    fn test_execute_selection_set_abstract_types() {
        let schema = with_supergraph_boilerplate(
            "type Query @join__type(graph: TEST){ hello: String @join__field(graph: TEST)}
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
        let schema = Schema::parse(&schema, &Default::default()).unwrap();

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
          @link(url: "https://specs.apollo.dev/link/v1.0")
          @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION) {
          query: Query
        }

        directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

        directive @join__field(
          graph: join__Graph
          requires: join__FieldSet
          provides: join__FieldSet
          type: String
          external: Boolean
          override: String
          usedOverridden: Boolean
        ) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

        directive @join__graph(name: String!, url: String!) on ENUM_VALUE

        directive @join__implements(
          graph: join__Graph!
          interface: String!
        ) repeatable on OBJECT | INTERFACE

        directive @join__type(
          graph: join__Graph!
          key: join__FieldSet
          extension: Boolean! = false
          resolvable: Boolean! = true
          isInterfaceObject: Boolean! = false
        ) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

        directive @join__unionMember(
          graph: join__Graph!
          member: String!
        ) repeatable on UNION

        directive @link(
          url: String
          as: String
          for: link__Purpose
          import: [link__Import]
        ) repeatable on SCHEMA

        scalar join__FieldSet

        enum join__Graph {
            TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
        }

        scalar link__Import

        enum link__Purpose {
          SECURITY
          EXECUTION
        }
        "#,
            content
        )
    }
}
