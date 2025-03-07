use apollo_compiler::executable::Field;
use apollo_compiler::executable::NamedType;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::Value;
use tower::BoxError;

use crate::Context;
use crate::plugins::telemetry::config_new::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selector;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::attributes::StandardAttribute;
use crate::plugins::telemetry::config_new::graphql::selectors::FieldName;
use crate::plugins::telemetry::config_new::graphql::selectors::FieldType;
use crate::plugins::telemetry::config_new::graphql::selectors::GraphQLSelector;
use crate::plugins::telemetry::config_new::graphql::selectors::ListLength;
use crate::plugins::telemetry::config_new::graphql::selectors::TypeName;
use crate::plugins::telemetry::config_new::selectors::OperationName;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::supergraph;

#[derive(Deserialize, JsonSchema, Clone, Default, Debug, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct GraphQLAttributes {
    /// The GraphQL field name
    #[serde(rename = "graphql.field.name")]
    pub(crate) field_name: Option<StandardAttribute>,
    /// The GraphQL field type
    #[serde(rename = "graphql.field.type")]
    pub(crate) field_type: Option<StandardAttribute>,
    /// If the field is a list, the length of the list
    #[serde(rename = "graphql.list.length")]
    pub(crate) list_length: Option<StandardAttribute>,
    /// The GraphQL operation name
    #[serde(rename = "graphql.operation.name")]
    pub(crate) operation_name: Option<StandardAttribute>,
    /// The GraphQL type name
    #[serde(rename = "graphql.type.name")]
    pub(crate) type_name: Option<StandardAttribute>,
}

impl DefaultForLevel for GraphQLAttributes {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        if let TelemetryDataKind::Metrics = kind {
            if let DefaultAttributeRequirementLevel::Required = requirement_level {
                self.field_name.get_or_insert(StandardAttribute::Bool(true));
                self.field_type.get_or_insert(StandardAttribute::Bool(true));
                self.type_name.get_or_insert(StandardAttribute::Bool(true));
            }
        }
    }
}

impl Selectors<supergraph::Request, supergraph::Response, crate::graphql::Response>
    for GraphQLAttributes
{
    fn on_request(&self, _request: &supergraph::Request) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_response(&self, _response: &supergraph::Response) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_error(&self, _error: &BoxError, _ctx: &Context) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_response_field(
        &self,
        attrs: &mut Vec<KeyValue>,
        ty: &NamedType,
        field: &Field,
        value: &Value,
        ctx: &Context,
    ) {
        if let Some(key) = self
            .field_name
            .as_ref()
            .and_then(|a| a.key(Key::from_static_str("graphql.field.name")))
        {
            if let Some(name) = (GraphQLSelector::FieldName {
                field_name: FieldName::String,
            })
            .on_response_field(ty, field, value, ctx)
            {
                attrs.push(KeyValue::new(key, name));
            }
        }
        if let Some(key) = self
            .field_type
            .as_ref()
            .and_then(|a| a.key(Key::from_static_str("graphql.field.type")))
        {
            if let Some(ty) = (GraphQLSelector::FieldType {
                field_type: FieldType::Name,
            })
            .on_response_field(ty, field, value, ctx)
            {
                attrs.push(KeyValue::new(key, ty));
            }
        }
        if let Some(key) = self
            .type_name
            .as_ref()
            .and_then(|a| a.key(Key::from_static_str("graphql.type.name")))
        {
            if let Some(ty) = (GraphQLSelector::TypeName {
                type_name: TypeName::String,
            })
            .on_response_field(ty, field, value, ctx)
            {
                attrs.push(KeyValue::new(key, ty));
            }
        }
        if let Some(key) = self
            .list_length
            .as_ref()
            .and_then(|a| a.key(Key::from_static_str("graphql.list.length")))
        {
            if let Some(length) = (GraphQLSelector::ListLength {
                list_length: ListLength::Value,
            })
            .on_response_field(ty, field, value, ctx)
            {
                attrs.push(KeyValue::new(key, length));
            }
        }
        if let Some(key) = self
            .operation_name
            .as_ref()
            .and_then(|a| a.key(Key::from_static_str("graphql.operation.name")))
        {
            if let Some(length) = (GraphQLSelector::OperationName {
                operation_name: OperationName::String,
                default: None,
            })
            .on_response_field(ty, field, value, ctx)
            {
                attrs.push(KeyValue::new(key, length));
            }
        }
    }
}

#[cfg(test)]
mod test {
    use serde_json_bytes::json;

    use crate::Context;
    use crate::context::OPERATION_NAME;
    use crate::plugins::telemetry::config_new::DefaultForLevel;
    use crate::plugins::telemetry::config_new::Selectors;
    use crate::plugins::telemetry::config_new::attributes::StandardAttribute;
    use crate::plugins::telemetry::config_new::test::field;
    use crate::plugins::telemetry::config_new::test::ty;

    #[test]
    fn test_default_for_level() {
        let mut attributes = super::GraphQLAttributes::default();
        attributes.defaults_for_level(
            super::DefaultAttributeRequirementLevel::Required,
            super::TelemetryDataKind::Metrics,
        );
        assert_eq!(attributes.field_name, Some(StandardAttribute::Bool(true)));
        assert_eq!(attributes.field_type, Some(StandardAttribute::Bool(true)));
        assert_eq!(attributes.type_name, Some(StandardAttribute::Bool(true)));
        assert_eq!(attributes.list_length, None);
        assert_eq!(attributes.operation_name, None);
    }

    #[test]
    fn test_on_response_field_non_list() {
        let attributes = super::GraphQLAttributes {
            field_name: Some(StandardAttribute::Bool(true)),
            field_type: Some(StandardAttribute::Bool(true)),
            list_length: Some(StandardAttribute::Bool(true)),
            operation_name: Some(StandardAttribute::Bool(true)),
            type_name: Some(StandardAttribute::Bool(true)),
        };
        let ctx = Context::default();
        let _ = ctx.insert(OPERATION_NAME, "operation_name".to_string());
        let mut result = Default::default();
        attributes.on_response_field(&mut result, &ty(), field(), &json!(true), &ctx);
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].key.as_str(), "graphql.field.name");
        assert_eq!(result[0].value.as_str(), "field_name");
        assert_eq!(result[1].key.as_str(), "graphql.field.type");
        assert_eq!(result[1].value.as_str(), "field_type");
        assert_eq!(result[2].key.as_str(), "graphql.type.name");
        assert_eq!(result[2].value.as_str(), "type_name");
        assert_eq!(result[3].key.as_str(), "graphql.operation.name");
        assert_eq!(result[3].value.as_str(), "operation_name");
    }

    #[test]
    fn test_on_response_field_list() {
        let attributes = super::GraphQLAttributes {
            field_name: Some(StandardAttribute::Bool(true)),
            field_type: Some(StandardAttribute::Bool(true)),
            list_length: Some(StandardAttribute::Bool(true)),
            operation_name: Some(StandardAttribute::Bool(true)),
            type_name: Some(StandardAttribute::Bool(true)),
        };
        let ctx = Context::default();
        let _ = ctx.insert(OPERATION_NAME, "operation_name".to_string());
        let mut result = Default::default();
        attributes.on_response_field(
            &mut result,
            &ty(),
            field(),
            &json!(vec![true, true, true]),
            &ctx,
        );
        assert_eq!(result.len(), 5);
        assert_eq!(result[0].key.as_str(), "graphql.field.name");
        assert_eq!(result[0].value.as_str(), "field_name");
        assert_eq!(result[1].key.as_str(), "graphql.field.type");
        assert_eq!(result[1].value.as_str(), "field_type");
        assert_eq!(result[2].key.as_str(), "graphql.type.name");
        assert_eq!(result[2].value.as_str(), "type_name");
        assert_eq!(result[3].key.as_str(), "graphql.list.length");
        assert_eq!(result[3].value.as_str(), "3");
        assert_eq!(result[4].key.as_str(), "graphql.operation.name");
        assert_eq!(result[4].value.as_str(), "operation_name");
    }
}
