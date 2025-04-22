use std::fmt::Debug;

use schemars::JsonSchema;
use serde::Deserialize;

use super::selectors::SupergraphSelector;
use super::selectors::SupergraphValue;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::cost::CostInstrumentsConfig;
use crate::plugins::telemetry::config_new::instruments::CustomInstruments;
use crate::plugins::telemetry::config_new::supergraph::attributes::SupergraphAttributes;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::supergraph;

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SupergraphInstrumentsConfig {
    #[serde(flatten)]
    pub(crate) cost: CostInstrumentsConfig,
}

impl DefaultForLevel for SupergraphInstrumentsConfig {
    fn defaults_for_level(
        &mut self,
        _requirement_level: DefaultAttributeRequirementLevel,
        _kind: TelemetryDataKind,
    ) {
    }
}

pub(crate) type SupergraphCustomInstruments = CustomInstruments<
    supergraph::Request,
    supergraph::Response,
    crate::graphql::Response,
    SupergraphAttributes,
    SupergraphSelector,
    SupergraphValue,
>;
