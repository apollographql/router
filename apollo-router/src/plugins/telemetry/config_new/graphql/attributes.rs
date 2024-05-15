use opentelemetry_api::KeyValue;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::telemetry::config_new::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::otlp::TelemetryDataKind;

#[derive(Deserialize, JsonSchema, Clone, Default, Debug, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct GraphQLAttributes {
    field_name: Option<bool>,
    type_name: Option<bool>,
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
    type Request = ();
    type Response = ();
    type EventResponse = ();

    fn on_request(&self, _request: &Self::Request) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_response(&self, _response: &Self::Response) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_response_field(
        &self,
        field: &apollo_compiler::ast::Field,
        _value: &serde_json::Value,
    ) -> Vec<KeyValue> {
        let mut attrs = Vec::with_capacity(2);
        if let Some(true) = self.field_name {
            attrs.push(KeyValue::new::<&str, String>(
                "field.name",
                field.name.to_string(),
            ));
        }
        if let Some(true) = self.type_name {
            // attrs.push(KeyValue::new("type.name", field.type_name().into()));
            todo!("Implement type name attribute")
        }
        attrs
    }

    fn on_error(&self, _error: &BoxError) -> Vec<KeyValue> {
        Vec::default()
    }
}
