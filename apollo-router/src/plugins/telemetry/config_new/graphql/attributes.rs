use opentelemetry_api::KeyValue;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::graphql::Request;
use crate::graphql::Response;
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
    type Request = Request;
    type Response = Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) -> Vec<KeyValue> {
        Vec::with_capacity(0)
    }

    fn on_response(&self, response: &Self::Response) -> Vec<KeyValue> {
        todo!()
    }

    fn on_error(&self, error: &BoxError) -> Vec<KeyValue> {
        Vec::with_capacity(0)
    }
}
