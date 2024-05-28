use apollo_compiler::ast::NamedType;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::ExecutableDocument;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;

use super::DemandControlError;
use crate::graphql::Response;

pub(crate) trait ResponseVisitor {
    fn visit(
        &mut self,
        request: &ExecutableDocument,
        response: &Response,
    ) -> Result<(), DemandControlError> {
        if let Some(Value::Object(children)) = &response.data {
            if let Some(operation) = &request.anonymous_operation {
                self.visit_selections(request, &operation.selection_set, children)?;
            }
            for operation in request.named_operations.values() {
                self.visit_selections(request, &operation.selection_set, children)?;
            }
            Ok(())
        } else {
            Err(DemandControlError::ResponseTypingFailure(
                "Can't zip an operation with a non-object!".to_string(),
            ))
        }
    }

    fn visit_selections(
        &mut self,
        request: &ExecutableDocument,
        selection_set: &SelectionSet,
        fields: &Map<ByteString, Value>,
    ) -> Result<(), DemandControlError> {
        for selection in &selection_set.selections {
            match selection {
                Selection::Field(inner_field) => {
                    if let Some(value) = fields.get(inner_field.name.as_str()) {
                        self.visit_field(request, &selection_set.ty, inner_field.as_ref(), value)?;
                    } else {
                        tracing::warn!("The response did not include a field corresponding to query field {:?}", inner_field);
                    }
                }
                Selection::FragmentSpread(fragment_spread) => {
                    if let Some(fragment) = fragment_spread.fragment_def(request) {
                        self.visit_selections(request, &fragment.selection_set, fields)?;
                    } else {
                        return Err(DemandControlError::ResponseTypingFailure(format!(
                            "The fragment {} was not found.",
                            fragment_spread.fragment_name
                        )));
                    }
                }
                Selection::InlineFragment(inline_fragment) => {
                    self.visit_selections(request, &inline_fragment.selection_set, fields)?;
                }
            }
        }

        Ok(())
    }

    fn visit_field(
        &mut self,
        request: &ExecutableDocument,
        ty: &NamedType,
        field: &Field,
        value: &Value,
    ) -> Result<(), DemandControlError>;
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;
    use bytes::Bytes;
    use insta::assert_yaml_snapshot;
    use serde::ser::SerializeMap;
    use serde::Serialize;
    use serde::Serializer;

    use super::*;
    use crate::graphql::Response;

    struct FieldCounter {
        counts: HashMap<String, usize>,
    }

    impl FieldCounter {
        fn new() -> Self {
            Self {
                counts: HashMap::new(),
            }
        }
    }

    impl ResponseVisitor for FieldCounter {
        fn visit_field(
            &mut self,
            request: &ExecutableDocument,
            ty: &NamedType,
            field: &Field,
            value: &Value,
        ) -> Result<(), DemandControlError> {
            match value {
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                    let count = self.counts.entry(field.name.to_string()).or_insert(0);
                    *count += 1;
                }
                Value::Array(items) => {
                    for item in items {
                        self.visit_field(request, ty, field, item)?;
                    }
                }
                Value::Object(children) => {
                    let count = self.counts.entry(field.name.to_string()).or_insert(0);
                    *count += 1;
                    self.visit_selections(request, &field.selection_set, children)?;
                }
            }

            Ok(())
        }
    }

    #[test]
    fn response_zipper() {
        let schema_str = include_str!("fixtures/federated_ships_schema.graphql");
        let query_str = include_str!("fixtures/federated_ships_required_query.graphql");
        let response_bytes = include_bytes!("fixtures/federated_ships_required_response.json");

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let request = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test", Bytes::from_static(response_bytes)).unwrap();

        let mut visitor = FieldCounter::new();
        visitor.visit(&request, &response).unwrap();
        insta::with_settings!({sort_maps=>true}, { assert_yaml_snapshot!(visitor) })
    }

    #[test]
    fn fragment_response_zipper() {
        let schema_str = include_str!("fixtures/federated_ships_schema.graphql");
        let query_str = include_str!("fixtures/federated_ships_fragment_query.graphql");
        let response_bytes = include_bytes!("fixtures/federated_ships_fragment_response.json");

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let request = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test", Bytes::from_static(response_bytes)).unwrap();

        let mut visitor = FieldCounter::new();
        visitor.visit(&request, &response).unwrap();
        insta::with_settings!({sort_maps=>true}, { assert_yaml_snapshot!(visitor) })
    }

    #[test]
    fn inline_fragment_zipper() {
        let schema_str = include_str!("fixtures/federated_ships_schema.graphql");
        let query_str = include_str!("fixtures/federated_ships_inline_fragment_query.graphql");
        let response_bytes = include_bytes!("fixtures/federated_ships_fragment_response.json");

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let request = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test", Bytes::from_static(response_bytes)).unwrap();

        let mut visitor = FieldCounter::new();
        visitor.visit(&request, &response).unwrap();
        insta::with_settings!({sort_maps=>true}, { assert_yaml_snapshot!(visitor) })
    }

    #[test]
    fn named_operation_zipper() {
        let schema_str = include_str!("fixtures/federated_ships_schema.graphql");
        let query_str = include_str!("fixtures/federated_ships_named_query.graphql");
        let response_bytes = include_bytes!("fixtures/federated_ships_named_response.json");

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let request = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test", Bytes::from_static(response_bytes)).unwrap();

        let mut visitor = FieldCounter::new();
        visitor.visit(&request, &response).unwrap();
        insta::with_settings!({sort_maps=>true}, { assert_yaml_snapshot!(visitor) })
    }

    impl Serialize for FieldCounter {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut map = serializer.serialize_map(Some(self.counts.len()))?;
            for (key, value) in &self.counts {
                map.serialize_entry(key, value)?;
            }
            map.end()
        }
    }
}
