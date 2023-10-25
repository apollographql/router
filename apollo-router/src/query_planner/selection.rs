use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Entry;

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

pub(crate) fn execute_selection_set(
    input_content: &Value,
    selections: &[Selection],
    schema: &Schema,
    //document: &ParsedDocument,
) -> Value {
    let content = match input_content.as_object() {
        Some(o) => o,
        None => return Value::Null,
    };

    let mut output = Object::with_capacity(selections.len());
    for selection in selections {
        println!(
            "execute_selection_set: selection {selection:?} from {}",
            serde_json::to_string(&content).unwrap(),
        );

        match selection {
            Selection::Field(Field {
                alias,
                name,
                selections,
            }) => {
                let name = alias.as_deref().unwrap_or(name.as_str()); //node.alias ? node.alias : node.name;

                match content.get_key_value(name) {
                    None => {
                        /*
                        // the behaviour here does not align with the gateway: we should instead assume that
                        // data is in the correct shape, and return a null (or even no value at all) on
                        // missing fields. If a field was missing, it should have been nullified,
                        // and if it was non nullable, the parent object would have been nullified.
                        // Unfortunately, we don't validate subgraph responses yet
                        continue;
                        */
                        return Value::Null;
                    }
                    Some((key, value)) => {
                        if let Some(elements) = value.as_array() {
                            let selected = elements
                                .iter()
                                .map(|element| match selections {
                                    Some(sels) => execute_selection_set(element, sels, schema),
                                    None => element.clone(),
                                })
                                .collect::<Vec<_>>();
                            output.insert(key.clone(), Value::Array(selected));
                        } else if let Some(sels) = selections {
                            output.insert(key.clone(), execute_selection_set(value, sels, schema));
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
                    let b = is_object_of_type(content, condition, schema);
                    println!(
                        "is_object_of_type({condition})={b} for {}",
                        serde_json::to_string(&content).unwrap(),
                    );
                    if is_object_of_type(content, condition, schema) {
                        //output.deep_merge(execute_selection_set(schema, content, selections))
                        if let Value::Object(selected) =
                            execute_selection_set(input_content, selections, schema)
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
        println!(
            "execute_selection_set: output is now: {}",
            serde_json::to_string(&output).unwrap(),
        );
    }

    Value::Object(output)
}

fn is_object_of_type(obj: &Object, condition: &str, schema: &Schema) -> bool {
    let typename = match obj.get("__typename").and_then(|v| v.as_str()) {
        None => return false,
        Some(t) => t,
    };

    if condition == typename {
        return true;
    }

    let object_type = match schema.definitions.types.get(typename) {
        None => return false,
        Some(t) => t,
    };

    let conditional_type = match schema.definitions.types.get(condition) {
        None => return false,
        Some(t) => t,
    };

    if conditional_type.is_interface() || conditional_type.is_union() {
        return (object_type.is_object() || object_type.is_interface())
            && schema.is_subtype(condition, typename);
    }

    false
}
/*

export function isObjectOfType(
  schema: GraphQLSchema,
  obj: Record<string, any>,
  typeCondition: string,
  defaultOnUnknownObjectType: boolean = false,
): boolean {
  const objTypename = obj['__typename'];
  if (!objTypename) {
    return defaultOnUnknownObjectType;
  }

  if (typeCondition === objTypename) {
    return true;
  }

  const type = schema.getType(objTypename);
  if (!type) {
    return false;
  }

  const conditionalType = schema.getType(typeCondition);
  if (!conditionalType) {
    return false;
  }

  if (isAbstractType(conditionalType)) {
    return (isObjectType(type) || isInterfaceType(type)) && schema.isSubType(conditionalType, type);
  }

  return false;
}
*/
pub(crate) fn select_object(
    content: &Object,
    selections: &[Selection],
    schema: &Schema,
) -> Result<Value, FetchError> {
    let mut output = Object::with_capacity(selections.len());
    for selection in selections {
        match selection {
            Selection::Field(field) => {
                let (key, value) = select_field(content, field, schema)?;

                if let Some(o) = output.get_mut(field.name.as_str()) {
                    o.deep_merge(value);
                } else {
                    output.insert(
                        key.map(|bytestring| bytestring.to_owned())
                            .unwrap_or_else(|| field.name.as_str().to_owned().into()),
                        value,
                    );
                }
            }
            Selection::InlineFragment(fragment) => {
                if let Value::Object(value) = select_inline_fragment(content, fragment, schema)? {
                    output.append(&mut value.to_owned())
                }
            }
        };
    }

    Ok(Value::Object(output))
}

fn select_field<'a>(
    content: &'a Object,
    field: &Field,
    schema: &Schema,
) -> Result<(Option<&'a ByteString>, Value), FetchError> {
    let res = match (
        content.get_key_value(field.name.as_str()),
        &field.selections,
    ) {
        (Some((k, v)), _) => select_value(v, field, schema).map(|opt| (Some(k), opt)), //opt.map(|v| (k, v))),
        //FIXME: if this was a non nullable field, we should not return it
        (None, _) => Ok((None, Value::Null)),
        /*)Err(FetchError::ExecutionFieldNotFound {
            field: field.name.to_owned(),
        }),*/
    };
    res
}

fn select_inline_fragment(
    content: &Object,
    fragment: &InlineFragment,
    schema: &Schema,
) -> Result<Value, FetchError> {
    match (&fragment.type_condition, &content.get("__typename")) {
        (Some(condition), Some(Value::String(typename))) => {
            if condition == typename || schema.is_subtype(condition, typename.as_str()) {
                select_object(content, &fragment.selections, schema)
            } else {
                Ok(Value::Object(Object::new()))
            }
        }
        (None, _) => select_object(content, &fragment.selections, schema),
        (_, None) => Err(FetchError::ExecutionFieldNotFound {
            field: "__typename".to_string(),
        }),
        (_, _) => Ok(Value::Object(Object::new())),
    }
}

fn select_value(content: &Value, field: &Field, schema: &Schema) -> Result<Value, FetchError> {
    match (content, &field.selections) {
        (Value::Object(child), Some(selections)) => select_object(child, selections, schema),
        (Value::Array(elements), Some(_)) => elements
            .iter()
            .map(|element| select_value(element, field, schema))
            .collect::<Result<Vec<Value>, FetchError>>()
            .map(|v| Value::Array(v)),
        (value, None) => Ok(value.to_owned()),
        _ => Ok(Value::Null),
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

        let res = Ok(Value::Array(
            values
                .into_iter()
                .map(|value| {
                    execute_selection_set(value, selections, schema)
                    /*match (value, selections) {
                    (Value::Object(content), requires) => {
                        select_object(content, requires, schema) //.transpose()
                    }
                    (_, _) => Err(FetchError::ExecutionInvalidContent {
                        reason: "not an object".to_string(),
                    }),*/
                })
                .collect::<Vec<_>>(),
        ));
        println!("select res: {res:?}");
        res
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

        let value = execute_selection_set(&response, &selection, &schema);
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
