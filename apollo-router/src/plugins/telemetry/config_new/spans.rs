use schemars::JsonSchema;
use serde::Deserialize;

use super::connector::spans::ConnectorSpans;
use super::router::spans::RouterSpans;
use super::subgraph::spans::SubgraphSpans;
use super::supergraph::spans::SupergraphSpans;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::plugins::telemetry::span_factory::SpanMode;

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Spans {
    /// Use new OpenTelemetry spec compliant span attributes or preserve existing. This will be defaulted in future to `spec_compliant`, eventually removed in future.
    pub(crate) mode: SpanMode,

    /// The attributes to include by default in spans based on their level as specified in the otel semantic conventions and Apollo documentation.
    pub(crate) default_attribute_requirement_level: DefaultAttributeRequirementLevel,

    /// Configuration of router spans.
    /// Log events inherit attributes from the containing span, so attributes configured here will be included on log events for a request.
    /// Router spans contain http request and response information and therefore contain http specific attributes.
    pub(crate) router: RouterSpans,

    /// Configuration of supergraph spans.
    /// Supergraph spans contain information about the graphql request and response and therefore contain graphql specific attributes.
    pub(crate) supergraph: SupergraphSpans,

    /// Attributes to include on the subgraph span.
    /// Subgraph spans contain information about the subgraph request and response and therefore contain subgraph specific attributes.
    pub(crate) subgraph: SubgraphSpans,

    /// Attributes to include on the connector span.
    /// Connector spans contain information about the connector request and response and therefore contain connector specific attributes.
    pub(crate) connector: ConnectorSpans,
}

impl Spans {
    /// Update the defaults for spans configuration regarding the `default_attribute_requirement_level`
    pub(crate) fn update_defaults(&mut self) {
        self.router.defaults_for_levels(
            self.default_attribute_requirement_level,
            TelemetryDataKind::Traces,
        );
        self.supergraph.defaults_for_levels(
            self.default_attribute_requirement_level,
            TelemetryDataKind::Traces,
        );
        self.subgraph.defaults_for_levels(
            self.default_attribute_requirement_level,
            TelemetryDataKind::Traces,
        );
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        for (name, custom) in &self.router.attributes.custom {
            custom
                .validate()
                .map_err(|err| format!("error for router span attribute {name:?}: {err}"))?;
        }
        for (name, custom) in &self.supergraph.attributes.custom {
            custom
                .validate()
                .map_err(|err| format!("error for supergraph span attribute {name:?}: {err}"))?;
        }
        for (name, custom) in &self.subgraph.attributes.custom {
            custom
                .validate()
                .map_err(|err| format!("error for subgraph span attribute {name:?}: {err}"))?;
        }

        Ok(())
    }
}
