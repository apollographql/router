use opentelemetry::KeyValue;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::Context;
use crate::plugins::telemetry::config_new::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::attributes::StandardAttribute;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::subgraph;

#[derive(Deserialize, JsonSchema, Clone, Default, Debug, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct CacheAttributes {
    /// Entity type
    #[serde(rename = "graphql.type.name")]
    pub(crate) entity_type: Option<StandardAttribute>,
}

impl DefaultForLevel for CacheAttributes {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        if let TelemetryDataKind::Metrics = kind {
            if let DefaultAttributeRequirementLevel::Required = requirement_level {
                self.entity_type
                    .get_or_insert(StandardAttribute::Bool(false));
            }
        }
    }
}

// Nothing to do here because we're using a trick because entity_type is related to CacheControl data we put in the context and for one request we have several entity types
// and so several metrics to generate it can't be done here
impl Selectors<subgraph::Request, subgraph::Response, ()> for CacheAttributes {
    fn on_request(&self, _request: &subgraph::Request) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_response(&self, _response: &subgraph::Response) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_error(&self, _error: &BoxError, _ctx: &Context) -> Vec<KeyValue> {
        Vec::default()
    }
}
