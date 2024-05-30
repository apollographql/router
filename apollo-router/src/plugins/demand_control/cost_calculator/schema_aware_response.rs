use std::collections::HashMap;

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

pub(crate) trait Visitor {
    fn visit(&self, value: &TypedValue) {
        match value {
            TypedValue::Null => self.visit_null(),
            TypedValue::Bool(ty, f, b) => self.visit_bool(ty, f, b),
            TypedValue::Number(ty, f, n) => self.visit_number(ty, f, n),
            TypedValue::String(ty, f, s) => self.visit_string(ty, f, s),
            TypedValue::List(ty, f, items) => self.visit_array(ty, f, items),
            TypedValue::Object(ty, f, children) => self.visit_object(ty, f, children),
            TypedValue::Root(children) => self.visit_root(children),
        }
    }
    fn visit_field(&self, _value: &TypedValue) {}
    fn visit_null(&self) {}
    fn visit_bool(&self, _ty: &NamedType, _field: &Field, _value: &bool) {}
    fn visit_number(&self, _ty: &NamedType, _field: &Field, _value: &serde_json::Number) {}
    fn visit_string(&self, _ty: &NamedType, _field: &Field, _value: &str) {}

    fn visit_array(&self, _ty: &NamedType, _field: &Field, items: &[TypedValue]) {
        for value in items.iter() {
            self.visit_array_element(value);
            self.visit(value);
        }
    }
    fn visit_array_element(&self, _value: &TypedValue) {}
    fn visit_object(
        &self,
        _ty: &NamedType,
        _field: &Field,
        children: &HashMap<String, TypedValue>,
    ) {
        for value in children.values() {
            self.visit_field(value);
            self.visit(value);
        }
    }
    fn visit_root(&self, children: &HashMap<String, TypedValue>) {
        for value in children.values() {
            self.visit_field(value);
            self.visit(value);
        }
    }
}

#[derive(Debug)]
pub(crate) enum TypedValue<'a> {
    Null,
    Bool(&'a NamedType, &'a Field, &'a bool),
    Number(&'a NamedType, &'a Field, &'a serde_json::Number),
    String(&'a NamedType, &'a Field, &'a str),
    List(&'a NamedType, &'a Field, Vec<TypedValue<'a>>),
    Object(&'a NamedType, &'a Field, HashMap<String, TypedValue<'a>>),
    Root(HashMap<String, TypedValue<'a>>),
}

impl<'a> TypedValue<'a> {
    fn zip_field(
        request: &'a ExecutableDocument,
        ty: &'a NamedType,
        field: &'a Field,
        value: &'a Value,
    ) -> Result<TypedValue<'a>, DemandControlError> {
        match value {
            Value::Null => Ok(TypedValue::Null),
            Value::Bool(b) => Ok(TypedValue::Bool(ty, field, b)),
            Value::Number(n) => Ok(TypedValue::Number(ty, field, n)),
            Value::String(s) => Ok(TypedValue::String(ty, field, s.as_str())),
            Value::Array(items) => {
                let mut typed_items = Vec::new();
                for item in items {
                    typed_items.push(TypedValue::zip_field(request, ty, field, item)?);
                }
                Ok(TypedValue::List(ty, field, typed_items))
            }
            Value::Object(children) => {
                let typed_children = Self::zip_selections(request, &field.selection_set, children)?;
                Ok(TypedValue::Object(ty, field, typed_children))
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
                            TypedValue::zip_field(
                                request,
                                &selection_set.ty,
                                inner_field.as_ref(),
                                value,
                            )?,
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
    use insta::assert_yaml_snapshot;
    use serde::ser::SerializeMap;
    use serde::Serialize;
    use serde::Serializer;

    use super::*;
    use crate::graphql::Response;

    #[test]
    fn response_zipper() {
        let schema_str = include_str!("fixtures/federated_ships_schema.graphql");
        let query_str = include_str!("fixtures/federated_ships_required_query.graphql");
        let response_bytes = include_bytes!("fixtures/federated_ships_required_response.json");

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let request = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test", Bytes::from_static(response_bytes)).unwrap();
        let zipped = SchemaAwareResponse::new(&request, &response);
        insta::with_settings!({sort_maps=>true}, { assert_yaml_snapshot!(zipped.expect("expected zipped response")) })
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
        insta::with_settings!({sort_maps=>true}, { assert_yaml_snapshot!(zipped.expect("expected zipped response")) })
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
        insta::with_settings!({sort_maps=>true}, { assert_yaml_snapshot!(zipped.expect("expected zipped response")) })
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
        insta::with_settings!({sort_maps=>true}, { assert_yaml_snapshot!(zipped.expect("expected zipped response")) })
    }

    impl Serialize for SchemaAwareResponse<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            self.value.serialize(serializer)
        }
    }

    impl Serialize for TypedValue<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            match self {
                TypedValue::Null => serializer.serialize_none(),
                TypedValue::Bool(ty, field, b) => {
                    let mut map = serializer.serialize_map(None)?;
                    map.serialize_entry("type", ty.as_str())?;
                    map.serialize_entry("field", &field.name)?;
                    map.serialize_entry("value", b)?;
                    map.end()
                }
                TypedValue::Number(ty, field, n) => {
                    let mut map = serializer.serialize_map(None)?;
                    map.serialize_entry("type", ty.as_str())?;
                    map.serialize_entry("field", &field.name)?;
                    map.serialize_entry("value", n)?;
                    map.end()
                }
                TypedValue::String(ty, field, s) => {
                    let mut map = serializer.serialize_map(None)?;
                    map.serialize_entry("type", ty.as_str())?;
                    map.serialize_entry("field", &field.name)?;
                    map.serialize_entry("value", s)?;
                    map.end()
                }
                TypedValue::List(ty, field, items) => {
                    let mut map = serializer.serialize_map(None)?;
                    map.serialize_entry("type", ty.as_str())?;
                    map.serialize_entry("field", &field.name)?;
                    map.serialize_entry("items", items)?;
                    map.end()
                }
                TypedValue::Object(ty, field, children) => {
                    let mut map = serializer.serialize_map(None)?;
                    map.serialize_entry("type", ty.as_str())?;
                    map.serialize_entry("field", &field.name)?;
                    map.serialize_entry("children", children)?;
                    map.end()
                }
                TypedValue::Root(children) => children.serialize(serializer),
            }
        }
    }
}
