use std::collections::HashMap;

use apollo_compiler::executable::Field;
use apollo_compiler::executable::Operation;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;

use super::DemandControlError;

#[derive(Debug)]
pub(crate) enum TypedValue<'a> {
    Null,
    Bool(WithField<'a, bool>),
    Number(WithField<'a, ()>),
    String(WithField<'a, String>),
    Array(TypedArray<'a>),
    Object(TypedObject<'a>),
    Query(Root<'a>),
}

#[derive(Debug)]
pub(crate) struct WithField<'a, T> {
    pub(crate) field: &'a Field,
    pub(crate) value: T,
}

#[derive(Debug)]
pub(crate) struct Root<'a> {
    pub(crate) children: HashMap<String, TypedValue<'a>>,
}

#[derive(Debug)]
pub(crate) struct TypedObject<'a> {
    pub(crate) field: &'a Field,
    pub(crate) children: HashMap<String, TypedValue<'a>>,
}

#[derive(Debug)]
pub(crate) struct TypedArray<'a> {
    pub(crate) field: &'a Field,
    pub(crate) items: Vec<TypedValue<'a>>,
}

impl<'a> TypedValue<'a> {
    pub(crate) fn for_operation(
        operation: &'a Operation,
        value: &Value,
    ) -> Result<TypedValue<'a>, DemandControlError> {
        if let Value::Object(children) = value {
            // Since selections aren't indexed, we have to loop over them one by one, using the
            // response's map as a lookup for the corresponding value.
            let mut typed_fields: HashMap<String, TypedValue> = HashMap::new();
            for selection in &operation.selection_set.selections {
                match selection {
                    Selection::Field(field) => {
                        if let Some(value) = children.get(field.name.as_str()) {
                            typed_fields
                                .insert(field.name.to_string(), TypedValue::zip(field, value)?);
                        } else {
                            todo!("The response is missing a field it's supposed to include from the query")
                        }
                    }
                    Selection::FragmentSpread(_) => todo!("Handle fragment spread"),
                    Selection::InlineFragment(_) => todo!("Handle inline fragment"),
                }
            }
            Ok(TypedValue::Query(Root {
                children: typed_fields,
            }))
        } else {
            Err(DemandControlError::ResponseTypingFailure(
                "Can't zip an operation with a non-object!".to_string(),
            ))
        }
    }

    fn zip(field: &'a Field, value: &Value) -> Result<TypedValue<'a>, DemandControlError> {
        match value {
            Value::Null => Ok(TypedValue::Null),
            Value::Bool(_) => todo!("bool"),
            Value::Number(n) => Ok(TypedValue::Number(WithField { field, value: () })),
            Value::String(s) => Ok(TypedValue::String(WithField {
                field,
                value: s.as_str().to_string(),
            })),
            Value::Array(items) => {
                let mut typed_items = Vec::new();
                for item in items {
                    typed_items.push(TypedValue::zip(field, item)?);
                }
                Ok(TypedValue::Array(TypedArray {
                    field,
                    items: typed_items,
                }))
            }
            Value::Object(children) => {
                let mut typed_children: HashMap<String, TypedValue> = HashMap::new();
                for selection in &field.selection_set.selections {
                    match selection {
                        Selection::Field(inner_field) => {
                            let value = children.get(inner_field.name.as_str()).unwrap();
                            typed_children.insert(
                                field.name.to_string(),
                                TypedValue::zip(inner_field.as_ref(), value)?,
                            );
                        }
                        Selection::FragmentSpread(_) => todo!("Handle fragment spread"),
                        Selection::InlineFragment(_) => todo!("Handle inline fragment"),
                    }
                }
                Ok(TypedValue::Object(TypedObject {
                    field,
                    children: typed_children,
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::{ExecutableDocument, Schema};
    use serde_json_bytes::json;

    use crate::{graphql::Response, plugins::demand_control::schema_aware_response::TypedValue};

    #[test]
    fn response_zipper() {
        let schema_str = include_str!("./fixtures/federated_ships_schema.graphql");
        let query_str = r#"
            {
                ships {
                    name
                }
            }
        "#;
        let response_body = json!({
            "data": {
                "ships": [
                    {
                        "name": "Boaty McBoatface"
                    },
                    {
                        "name": "HMS Grapherson"
                    }
                ]
            }
        });

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let query = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test_service", response_body.to_bytes()).unwrap();

        println!("{:?}\n", query);
        println!("{:?}\n", response.data);

        let operation = query.anonymous_operation.unwrap();
        let zipped =
            TypedValue::for_operation(operation.as_ref(), &response.data.unwrap()).unwrap();
        println!("{:?}\n", zipped);

        assert_eq!(1.0, 2.0)
    }
}
