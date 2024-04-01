use std::collections::HashMap;

use apollo_compiler::executable::Field;
use apollo_compiler::executable::Operation;
use apollo_compiler::executable::Selection;
use serde_json_bytes::Value;

use crate::graphql::Response;

use super::DemandControlError;

pub(crate) struct SchemaAwareResponse<'a> {
    pub(crate) operation: &'a Operation,
    pub(crate) response: &'a Response,
    pub(crate) value: TypedValue<'a>,
}

impl<'a> SchemaAwareResponse<'a> {
    pub(crate) fn zip(
        operation: &'a Operation,
        response: &'a Response,
    ) -> Result<Self, DemandControlError> {
        if let Some(Value::Object(children)) = &response.data {
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
            Ok(Self {
                operation,
                response,
                value: TypedValue::Query(typed_fields),
            })
        } else {
            Err(DemandControlError::ResponseTypingFailure(
                "Can't zip an operation with a non-object!".to_string(),
            ))
        }
    }
}

#[derive(Debug)]
pub(crate) enum TypedValue<'a> {
    Null,
    Bool(&'a Field, &'a bool),
    Number(&'a Field, &'a serde_json::Number),
    String(&'a Field, &'a str),
    Array(&'a Field, Vec<TypedValue<'a>>),
    Object(&'a Field, HashMap<String, TypedValue<'a>>),
    Query(HashMap<String, TypedValue<'a>>),
}

impl<'a> TypedValue<'a> {
    fn zip(field: &'a Field, value: &'a Value) -> Result<TypedValue<'a>, DemandControlError> {
        match value {
            Value::Null => Ok(TypedValue::Null),
            Value::Bool(b) => Ok(TypedValue::Bool(field, b)),
            Value::Number(n) => Ok(TypedValue::Number(field, n)),
            Value::String(s) => Ok(TypedValue::String(field, s.as_str())),
            Value::Array(items) => {
                let mut typed_items = Vec::new();
                for item in items {
                    typed_items.push(TypedValue::zip(field, item)?);
                }
                Ok(TypedValue::Array(field, typed_items))
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
                Ok(TypedValue::Object(field, typed_children))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;
    use serde_json_bytes::json;

    use crate::graphql::Response;
    use crate::plugins::demand_control::schema_aware_response::SchemaAwareResponse;

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
        let zipped = SchemaAwareResponse::zip(&operation, &response);

        assert!(zipped.is_ok())
    }
}
