use crate::plugins::demand_control::cost_calculator::schema_aware_response::TypedValue;
use crate::Context;
use opentelemetry_api::KeyValue;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::telemetry::config_new::graphql::selectors::{
    FieldName, FieldType, GraphQLSelector, ListLength, TypeName,
};
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::{DefaultAttributeRequirementLevel, Selector};
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::supergraph;

#[derive(Deserialize, JsonSchema, Clone, Default, Debug, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct GraphQLAttributes {
    /// The GraphQL field name
    #[serde(rename = "graphql.field.name")]
    pub(crate) field_name: Option<bool>,
    /// The GraphQL field type
    #[serde(rename = "graphql.field.type")]
    pub(crate) field_type: Option<bool>,
    /// If the field is a list, the length of the list
    #[serde(rename = "graphql.list.length")]
    pub(crate) field_list_length: Option<bool>,
    /// The GraphQL type name
    #[serde(rename = "graphql.type.name")]
    pub(crate) type_name: Option<bool>,
}

impl DefaultForLevel for GraphQLAttributes {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        if let TelemetryDataKind::Metrics = kind {
            if let DefaultAttributeRequirementLevel::Required = requirement_level {
                self.field_name.get_or_insert(true);
                self.field_type.get_or_insert(true);
                self.type_name.get_or_insert(true);
            }
        }
    }
}

impl Selectors for GraphQLAttributes {
    type Request = supergraph::Request;
    type Response = supergraph::Response;
    type EventResponse = crate::graphql::Response;

    fn on_request(&self, _request: &Self::Request) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_response(&self, _response: &Self::Response) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_error(&self, _error: &BoxError) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_response_field(&self, typed_value: &TypedValue, ctx: &Context) -> Vec<KeyValue> {
        let mut attrs = Vec::with_capacity(4);
        if let Some(true) = self.field_name {
            if let Some(name) = (GraphQLSelector::FieldName {
                field_name: FieldName::String,
            })
            .on_response_field(typed_value, ctx)
            {
                attrs.push(KeyValue::new("graphql.field.name", name));
            }
        }
        if let Some(true) = self.field_type {
            if let Some(ty) = (GraphQLSelector::FieldType {
                field_type: FieldType::Name,
            })
            .on_response_field(typed_value, ctx)
            {
                attrs.push(KeyValue::new("graphql.field.type", ty));
            }
        }
        if let Some(true) = self.field_list_length {
            if let Some(length) = (GraphQLSelector::ListLength {
                list_length: ListLength::Value,
            })
            .on_response_field(typed_value, ctx)
            {
                attrs.push(KeyValue::new("graphql.list.length", length));
            }
        }
        if let Some(true) = self.type_name {
            if let Some(ty) = (GraphQLSelector::TypeName {
                type_name: TypeName::String,
            })
            .on_response_field(typed_value, ctx)
            {
                attrs.push(KeyValue::new("graphql.type.name", ty));
            }
        }
        attrs
    }
}
