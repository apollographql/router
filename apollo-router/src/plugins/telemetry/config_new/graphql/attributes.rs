use apollo_compiler::executable::Field;
use opentelemetry_api::KeyValue;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::demand_control::cost_calculator::schema_aware_response::TypedValue;
use crate::plugins::telemetry::config_new::graphql::selectors::{
    FieldLength, FieldName, FieldType, GraphQLSelector, TypeName,
};
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::{DefaultAttributeRequirementLevel, Selector};
use crate::plugins::telemetry::otlp::TelemetryDataKind;

#[derive(Deserialize, JsonSchema, Clone, Default, Debug, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct GraphQLAttributes {
    #[serde(rename = "graphql.field.name")]
    pub(crate) field_name: Option<bool>,
    #[serde(rename = "graphql.field.type")]
    pub(crate) field_type: Option<bool>,
    #[serde(rename = "graphql.field.length")]
    pub(crate) field_length: Option<bool>,
    #[serde(rename = "graphql.type.name")]
    pub(crate) type_name: Option<bool>,
}

impl DefaultForLevel for GraphQLAttributes {
    fn defaults_for_level(
        &mut self,
        _requirement_level: DefaultAttributeRequirementLevel,
        _kind: TelemetryDataKind,
    ) {
        // No-op?
    }
}

impl Selectors for GraphQLAttributes {
    type Request = Field;
    type Response = TypedValue;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) -> Vec<KeyValue> {
        let mut attrs = Vec::with_capacity(4);
        if let Some(true) = self.field_name {
            if let Some(name) = (GraphQLSelector::FieldName {
                field_name: FieldName::String,
            })
            .on_request(request)
            {
                attrs.push(KeyValue::new("graphql.field.name", name));
            }
        }
        if let Some(true) = self.field_type {
            if let Some(ty) = (GraphQLSelector::FieldType {
                field_type: FieldType::String,
            })
            .on_request(request)
            {
                attrs.push(KeyValue::new("graphql.field.type", ty));
            }
        }
        if let Some(true) = self.field_length {
            if let Some(length) = (GraphQLSelector::FieldLength {
                field_length: FieldLength::Value,
            })
            .on_request(request)
            {
                attrs.push(KeyValue::new("graphql.field.length", length));
            }
        }
        if let Some(true) = self.type_name {
            if let Some(ty) = (GraphQLSelector::TypeName {
                type_name: TypeName::String,
            })
            .on_request(request)
            {
                attrs.push(KeyValue::new("graphql.type.name", ty));
            }
        }
        attrs
    }

    fn on_response(&self, _response: &Self::Response) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_error(&self, _error: &BoxError) -> Vec<KeyValue> {
        Vec::default()
    }
}
