use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::conditional::Conditional;
use crate::plugins::telemetry::config_new::connector::http::attributes::ConnectorHttpAttributes;
use crate::plugins::telemetry::config_new::connector::http::selectors::ConnectorHttpSelector;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::otlp::TelemetryDataKind;

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ConnectorSpans {
    /// Custom attributes that are attached to the connector span.
    pub(crate) attributes: Extendable<ConnectorHttpAttributes, Conditional<ConnectorHttpSelector>>,
}

impl DefaultForLevel for ConnectorSpans {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        self.attributes.defaults_for_level(requirement_level, kind);
    }
}
