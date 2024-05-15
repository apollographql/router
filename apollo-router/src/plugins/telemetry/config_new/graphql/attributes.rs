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
    #[serde(rename = "field.name")]
    pub(crate) field_name: Option<String>,
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
        _field: &apollo_compiler::ast::Field,
        _value: &serde_json::Value,
    ) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_error(&self, _error: &BoxError) -> Vec<KeyValue> {
        Vec::default()
    }
}
