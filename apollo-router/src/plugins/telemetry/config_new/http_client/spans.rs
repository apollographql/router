use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::conditional::Conditional;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::http_client::attributes::HttpClientAttributes;
use crate::plugins::telemetry::config_new::http_client::selectors::HttpClientSelector;
use crate::plugins::telemetry::otlp::TelemetryDataKind;

#[derive(Deserialize, JsonSchema, Clone, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct HttpClientSpans {
    /// Custom attributes that are attached to the HTTP client span.
    pub(crate) attributes: Extendable<HttpClientAttributes, Conditional<HttpClientSelector>>,
}

impl DefaultForLevel for HttpClientSpans {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        self.attributes.defaults_for_level(requirement_level, kind);
    }
}
