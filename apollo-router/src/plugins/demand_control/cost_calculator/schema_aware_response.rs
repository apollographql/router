use std::collections::HashMap;

use apollo_compiler::executable::Field;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::ExecutableDocument;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;

use super::DemandControlError;
use crate::graphql::Response;

pub(crate) struct SchemaAwareResponse<'a> {
    pub(crate) value: TypedValue<'a>,
}

impl<'a> SchemaAwareResponse<'a> {
    pub(crate) fn new(
        request: &'a ExecutableDocument,
        response: &'a Response,
    ) -> Result<Self, DemandControlError> {
        if let Some(Value::Object(children)) = &response.data {
            let mut typed_children: HashMap<String, TypedValue<'a>> = HashMap::new();

            if let Some(operation) = &request.anonymous_operation {
                typed_children.extend(TypedValue::zip_selections(
                    request,
                    &operation.selection_set,
                    children,
                )?);
            }
            for operation in request.named_operations.values() {
                typed_children.extend(TypedValue::zip_selections(
                    request,
                    &operation.selection_set,
                    children,
                )?);
            }

            Ok(Self {
                value: TypedValue::Root(typed_children),
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
    Root(HashMap<String, TypedValue<'a>>),
}

impl<'a> TypedValue<'a> {
    fn zip_field(
        request: &'a ExecutableDocument,
        field: &'a Field,
        value: &'a Value,
    ) -> Result<TypedValue<'a>, DemandControlError> {
        match value {
            Value::Null => Ok(TypedValue::Null),
            Value::Bool(b) => Ok(TypedValue::Bool(field, b)),
            Value::Number(n) => Ok(TypedValue::Number(field, n)),
            Value::String(s) => Ok(TypedValue::String(field, s.as_str())),
            Value::Array(items) => {
                let mut typed_items = Vec::new();
                for item in items {
                    typed_items.push(TypedValue::zip_field(request, field, item)?);
                }
                Ok(TypedValue::Array(field, typed_items))
            }
            Value::Object(children) => {
                let typed_children = Self::zip_selections(request, &field.selection_set, children)?;
                Ok(TypedValue::Object(field, typed_children))
            }
        }
    }

    fn zip_selections(
        request: &'a ExecutableDocument,
        selection_set: &'a SelectionSet,
        fields: &'a Map<ByteString, Value>,
    ) -> Result<HashMap<String, TypedValue<'a>>, DemandControlError> {
        let mut typed_children: HashMap<String, TypedValue> = HashMap::new();
        for selection in &selection_set.selections {
            match selection {
                Selection::Field(inner_field) => {
                    if let Some(value) = fields.get(inner_field.name.as_str()) {
                        typed_children.insert(
                            inner_field.name.to_string(),
                            TypedValue::zip_field(request, inner_field.as_ref(), value)?,
                        );
                    } else {
                        tracing::warn!("The response did not include a field corresponding to query field {:?}", inner_field);
                    }
                }
                Selection::FragmentSpread(fragment_spread) => {
                    if let Some(fragment) = fragment_spread.fragment_def(request) {
                        typed_children.extend(Self::zip_selections(
                            request,
                            &fragment.selection_set,
                            fields,
                        )?);
                    } else {
                        return Err(DemandControlError::ResponseTypingFailure(format!(
                            "The fragment {} was not found.",
                            fragment_spread.fragment_name
                        )));
                    }
                }
                Selection::InlineFragment(inline_fragment) => {
                    typed_children.extend(Self::zip_selections(
                        request,
                        &inline_fragment.selection_set,
                        fields,
                    )?);
                }
            }
        }
        Ok(typed_children)
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;
    use bytes::Bytes;

    use crate::graphql::Response;
    use crate::plugins::demand_control::cost_calculator::schema_aware_response::SchemaAwareResponse;

    #[test]
    fn response_zipper() {
        let schema_str = include_str!("fixtures/federated_ships_schema.graphql");
        let query_str = include_str!("fixtures/federated_ships_required_query.graphql");
        let response_bytes = include_bytes!("fixtures/federated_ships_required_response.json");

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let request = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test", Bytes::from_static(response_bytes)).unwrap();
        let zipped = SchemaAwareResponse::new(&request, &response);

        assert!(zipped.is_ok())
    }

    #[test]
    fn fragment_response_zipper() {
        let schema_str = include_str!("fixtures/federated_ships_schema.graphql");
        let query_str = include_str!("fixtures/federated_ships_fragment_query.graphql");
        let response_bytes = include_bytes!("fixtures/federated_ships_fragment_response.json");

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let request = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test", Bytes::from_static(response_bytes)).unwrap();
        let zipped = SchemaAwareResponse::new(&request, &response);

        assert!(zipped.is_ok())
    }

    #[test]
    fn inline_fragment_zipper() {
        let schema_str = include_str!("fixtures/federated_ships_schema.graphql");
        let query_str = include_str!("fixtures/federated_ships_inline_fragment_query.graphql");
        let response_bytes = include_bytes!("fixtures/federated_ships_fragment_response.json");

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let request = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test", Bytes::from_static(response_bytes)).unwrap();
        let zipped = SchemaAwareResponse::new(&request, &response);

        assert!(zipped.is_ok())
    }

    #[test]
    fn named_operation_zipper() {
        let schema_str = include_str!("fixtures/federated_ships_schema.graphql");
        let query_str = include_str!("fixtures/federated_ships_named_query.graphql");
        let response_bytes = include_bytes!("fixtures/federated_ships_named_response.json");

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let request = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test", Bytes::from_static(response_bytes)).unwrap();
        let zipped = SchemaAwareResponse::new(&request, &response);

        assert!(zipped.is_ok())
    }
}
