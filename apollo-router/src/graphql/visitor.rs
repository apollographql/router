use serde_json_bytes::Value;

use crate::graphql::Response;
use crate::json_ext::Object;

pub(crate) trait ResponseVisitor {
    fn visit_field(
        &mut self,
        request: &apollo_compiler::ExecutableDocument,
        variables: &Object,
        _ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &Value,
    ) {
        match value {
            Value::Array(items) => {
                for item in items {
                    self.visit_list_item(
                        request,
                        variables,
                        field.ty().inner_named_type(),
                        field,
                        item,
                    );
                }
            }
            Value::Object(children) => {
                self.visit_selections(request, variables, &field.selection_set, children);
            }
            _ => {}
        }
    }

    fn visit_list_item(
        &mut self,
        request: &apollo_compiler::ExecutableDocument,
        variables: &Object,
        _ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &Value,
    ) {
        match value {
            Value::Array(items) => {
                for item in items {
                    self.visit_list_item(request, variables, _ty, field, item);
                }
            }
            Value::Object(children) => {
                self.visit_selections(request, variables, &field.selection_set, children);
            }
            _ => {}
        }
    }

    fn visit(
        &mut self,
        request: &apollo_compiler::ExecutableDocument,
        response: &Response,
        variables: &Object,
    ) {
        if response.path.is_some() {
            // TODO: In this case, we need to find the selection inside `request` corresponding to the path so we can start zipping.
            // Exiting here means any implementing visitor will not operate on deffered responses.
            return;
        }

        if let Some(Value::Object(children)) = &response.data {
            if let Some(operation) = &request.operations.anonymous {
                self.visit_selections(request, variables, &operation.selection_set, children);
            }
            for operation in request.operations.named.values() {
                self.visit_selections(request, variables, &operation.selection_set, children);
            }
        }
    }

    fn visit_selections(
        &mut self,
        request: &apollo_compiler::ExecutableDocument,
        variables: &Object,
        selection_set: &apollo_compiler::executable::SelectionSet,
        fields: &Object,
    ) {
        for selection in &selection_set.selections {
            match selection {
                apollo_compiler::executable::Selection::Field(inner_field) => {
                    if let Some(value) = fields.get(inner_field.name.as_str()) {
                        self.visit_field(
                            request,
                            variables,
                            &selection_set.ty,
                            inner_field.as_ref(),
                            value,
                        );
                    } else {
                        tracing::warn!("The response did not include a field corresponding to query field {:?}", inner_field);
                    }
                }
                apollo_compiler::executable::Selection::FragmentSpread(fragment_spread) => {
                    if let Some(fragment) = fragment_spread.fragment_def(request) {
                        self.visit_selections(request, variables, &fragment.selection_set, fields);
                    } else {
                        tracing::warn!(
                            "The fragment {} was not found in the query document.",
                            fragment_spread.fragment_name
                        );
                    }
                }
                apollo_compiler::executable::Selection::InlineFragment(inline_fragment) => {
                    self.visit_selections(
                        request,
                        variables,
                        &inline_fragment.selection_set,
                        fields,
                    );
                }
            }
        }
    }
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

    #[test]
    fn test_visit_response() {
        let schema_str = include_str!("fixtures/federated_ships_schema.graphql");
        let query_str = include_str!("fixtures/federated_ships_required_query.graphql");
        let response_bytes = include_bytes!("fixtures/federated_ships_required_response.json");

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let request = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test", Bytes::from_static(response_bytes)).unwrap();

        let mut visitor = FieldCounter::new();
        visitor.visit(&request, &response, &Default::default());
        insta::with_settings!({sort_maps=>true}, { assert_yaml_snapshot!(visitor) })
    }

    #[test]
    fn test_visit_response_with_fragments() {
        let schema_str = include_str!("fixtures/federated_ships_schema.graphql");
        let query_str = include_str!("fixtures/federated_ships_fragment_query.graphql");
        let response_bytes = include_bytes!("fixtures/federated_ships_fragment_response.json");

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let request = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test", Bytes::from_static(response_bytes)).unwrap();

        let mut visitor = FieldCounter::new();
        visitor.visit(&request, &response, &Default::default());
        insta::with_settings!({sort_maps=>true}, { assert_yaml_snapshot!(visitor) })
    }

    #[test]
    fn test_visit_response_with_inline_fragments() {
        let schema_str = include_str!("fixtures/federated_ships_schema.graphql");
        let query_str = include_str!("fixtures/federated_ships_inline_fragment_query.graphql");
        let response_bytes = include_bytes!("fixtures/federated_ships_fragment_response.json");

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let request = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test", Bytes::from_static(response_bytes)).unwrap();

        let mut visitor = FieldCounter::new();
        visitor.visit(&request, &response, &Default::default());
        insta::with_settings!({sort_maps=>true}, { assert_yaml_snapshot!(visitor) })
    }

    #[test]
    fn test_visit_response_with_named_operation() {
        let schema_str = include_str!("fixtures/federated_ships_schema.graphql");
        let query_str = include_str!("fixtures/federated_ships_named_query.graphql");
        let response_bytes = include_bytes!("fixtures/federated_ships_named_response.json");

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let request = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test", Bytes::from_static(response_bytes)).unwrap();

        let mut visitor = FieldCounter::new();
        visitor.visit(&request, &response, &Default::default());
        insta::with_settings!({sort_maps=>true}, { assert_yaml_snapshot!(visitor) })
    }

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
            variables: &Object,
            _ty: &apollo_compiler::executable::NamedType,
            field: &apollo_compiler::executable::Field,
            value: &Value,
        ) {
            let count = self.counts.entry(field.name.to_string()).or_insert(0);
            *count += 1;
            match value {
                Value::Array(items) => {
                    for item in items {
                        self.visit_list_item(
                            request,
                            variables,
                            field.ty().inner_named_type(),
                            field,
                            item,
                        );
                    }
                }
                Value::Object(children) => {
                    self.visit_selections(request, variables, &field.selection_set, children);
                }
                _ => {}
            }
        }
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
