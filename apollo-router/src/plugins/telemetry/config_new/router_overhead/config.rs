use opentelemetry::KeyValue;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use super::RouterOverheadTracker;
use crate::Context;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::router;

/// Empty attributes struct for router overhead - no standard attributes, only custom selectors
#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct RouterOverheadAttributes {}

impl Selectors<router::Request, router::Response, ()> for RouterOverheadAttributes {
    fn on_request(&self, _request: &router::Request) -> Vec<KeyValue> {
        Vec::new()
    }

    fn on_response(&self, response: &router::Response) -> Vec<KeyValue> {
        response
            .context
            .extensions()
            .with_lock(|ext| ext.get::<RouterOverheadTracker>().cloned())
            .map(|tracker| {
                let result = tracker.calculate_overhead();
                vec![KeyValue::new(
                    "subgraph.active_requests",
                    result.active_subgraph_requests > 0,
                )]
            })
            .unwrap_or_default()
    }

    fn on_error(&self, _error: &BoxError, _ctx: &Context) -> Vec<KeyValue> {
        Vec::new()
    }
}

impl DefaultForLevel for RouterOverheadAttributes {
    fn defaults_for_level(
        &mut self,
        _requirement_level: DefaultAttributeRequirementLevel,
        _kind: TelemetryDataKind,
    ) {
        // No standard attributes to configure
    }
}
