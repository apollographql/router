use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::selectors::RouterSelector;
use crate::plugins::telemetry::config_new::selectors::SubgraphSelector;
use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
use crate::plugins::telemetry::config_new::DefaultForLevel;
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
}

impl Spans {
    /// Update the defaults for spans configuration regarding the `default_attribute_requirement_level`
    pub(crate) fn update_defaults(&mut self) {
        self.router
            .defaults_for_level(&self.default_attribute_requirement_level);
        self.supergraph
            .defaults_for_level(&self.default_attribute_requirement_level);
        self.subgraph
            .defaults_for_level(&self.default_attribute_requirement_level);
    }
}

#[derive(Deserialize, JsonSchema, Clone, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct RouterSpans {
    /// Custom attributes that are attached to the router span.
    pub(crate) attributes: Extendable<RouterAttributes, RouterSelector>,
}

#[derive(Deserialize, JsonSchema, Clone, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SupergraphSpans {
    /// Custom attributes that are attached to the supergraph span.
    pub(crate) attributes: Extendable<SupergraphAttributes, SupergraphSelector>,
}

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubgraphSpans {
    /// Custom attributes that are attached to the subgraph span.
    pub(crate) attributes: Extendable<SubgraphAttributes, SubgraphSelector>,
}

impl DefaultForLevel for RouterSpans {
    fn defaults_for_level(&mut self, requirement_level: &DefaultAttributeRequirementLevel) {
        self.attributes
            .attributes
            .common
            .defaults_for_level(requirement_level);
    }
}

impl DefaultForLevel for SupergraphSpans {
    fn defaults_for_level(&mut self, requirement_level: &DefaultAttributeRequirementLevel) {
        match requirement_level {
            DefaultAttributeRequirementLevel::Required => {}
            DefaultAttributeRequirementLevel::Recommended => {
                if self.attributes.attributes.graphql_document.is_none() {
                    self.attributes.attributes.graphql_document = Some(true);
                }
                if self.attributes.attributes.graphql_operation_name.is_none() {
                    self.attributes.attributes.graphql_operation_name = Some(true);
                }
                if self.attributes.attributes.graphql_operation_type.is_none() {
                    self.attributes.attributes.graphql_operation_type = Some(true);
                }
            }
            DefaultAttributeRequirementLevel::None => {}
        }
    }
}

impl DefaultForLevel for SubgraphSpans {
    fn defaults_for_level(&mut self, requirement_level: &DefaultAttributeRequirementLevel) {
        match requirement_level {
            DefaultAttributeRequirementLevel::Required => {
                if self
                    .attributes
                    .attributes
                    .graphql_federation_subgraph_name
                    .is_none()
                {
                    self.attributes.attributes.graphql_federation_subgraph_name = Some(true);
                }
            }
            DefaultAttributeRequirementLevel::Recommended => {
                if self
                    .attributes
                    .attributes
                    .graphql_federation_subgraph_name
                    .is_none()
                {
                    self.attributes.attributes.graphql_federation_subgraph_name = Some(true);
                }
                if self.attributes.attributes.graphql_document.is_none() {
                    self.attributes.attributes.graphql_document = Some(true);
                }
                if self.attributes.attributes.graphql_operation_name.is_none() {
                    self.attributes.attributes.graphql_operation_name = Some(true);
                }
                if self.attributes.attributes.graphql_operation_type.is_none() {
                    self.attributes.attributes.graphql_operation_type = Some(true);
                }
            }
            DefaultAttributeRequirementLevel::None => {}
        }
    }
}
