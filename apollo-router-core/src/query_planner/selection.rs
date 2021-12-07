use crate::prelude::graphql::*;
use serde::Deserialize;
use serde_json::map::Entry;

/// A selection that is part of a fetch.
/// Selections are used to propagate data to subgraph fetches.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "PascalCase", tag = "kind")]
pub(crate) enum Selection {
    /// A field selection.
    Field(Field),

    /// An inline fragment selection.
    InlineFragment(InlineFragment),
}

/// The field that is used
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Field {
    /// An optional alias for the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    alias: Option<String>,

    /// The name of the field.
    name: String,

    /// The selections for the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    selections: Option<Vec<Selection>>,
}

/// An inline fragment.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InlineFragment {
    /// The required fragment type.
    #[serde(skip_serializing_if = "Option::is_none")]
    type_condition: Option<String>,

    /// The selections from the fragment.
    selections: Vec<Selection>,
}

//FIXME: we need to return errors on invalid fetches here
pub(crate) fn select_values<'a>(
    path: &'a Path,
    data: &'a Value,
) -> Result<Vec<(Path, &'a Value)>, FetchError> {
    let mut res = Vec::new();
    match iterate_path(&Path::default(), &path.0, data, &mut res) {
        Some(err) => Err(err),
        None => Ok(res),
    }
}

fn iterate_path<'a>(
    parent: &Path,
    path: &'a [PathElement],
    data: &'a Value,
    results: &mut Vec<(Path, &'a Value)>,
) -> Option<FetchError> {
    match path.get(0) {
        None => {
            results.push((parent.clone(), data));
            None
        }
        Some(PathElement::Flatten) => match data.as_array() {
            None => Some(FetchError::ExecutionInvalidContent {
                reason: "not an array".to_string(),
            }),
            Some(array) => {
                for (i, value) in array.into_iter().enumerate() {
                    if let Some(err) = iterate_path(
                        &parent.join(Path::from(i.to_string())),
                        &path[1..],
                        value,
                        results,
                    ) {
                        return Some(err);
                    }
                }
                None
            }
        },
        Some(PathElement::Index(i)) => {
            if let Value::Array(a) = data {
                if let Some(value) = a.get(*i) {
                    iterate_path(
                        &parent.join(Path::from(i.to_string())),
                        &path[1..],
                        value,
                        results,
                    )
                } else {
                    Some(FetchError::ExecutionPathNotFound {
                        reason: format!("index {} not found", i),
                    })
                }
            } else {
                Some(FetchError::ExecutionInvalidContent {
                    reason: "not an array".to_string(),
                })
            }
        }
        Some(PathElement::Key(k)) => {
            if let Value::Object(o) = data {
                if let Some(value) = o.get(k) {
                    iterate_path(&parent.join(Path::from(k)), &path[1..], value, results)
                } else {
                    Some(FetchError::ExecutionPathNotFound {
                        reason: format!("key {} not found", k),
                    })
                }
            } else {
                Some(FetchError::ExecutionInvalidContent {
                    reason: "not an object".to_string(),
                })
            }
        }
    }
}

pub(crate) fn select_object(
    content: &Object,
    selections: &[Selection],
    schema: &Schema,
) -> Result<Option<Value>, FetchError> {
    let mut output = Object::new();
    for selection in selections {
        match selection {
            Selection::Field(field) => {
                if let Some(value) = select_field(content, field, schema)? {
                    match output.entry(field.name.to_owned()) {
                        Entry::Occupied(mut existing) => existing.get_mut().deep_merge(value),
                        Entry::Vacant(vacant) => {
                            vacant.insert(value);
                        }
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
    use super::Selection;
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
            select(&response, &Path::empty(), &selection, &schema)
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
}
