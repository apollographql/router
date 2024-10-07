use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::telemetry::config_new::connector::http::attributes::ConnectorHttpAttributes;
use crate::plugins::telemetry::config_new::connector::http::instruments::ConnectorHttpInstrumentsConfig;
use crate::plugins::telemetry::config_new::connector::http::selectors::ConnectorHttpSelector;
use crate::plugins::telemetry::config_new::connector::http::selectors::ConnectorHttpValue;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::Instrument;

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ConnectorInstrumentsKind {
    // TODO: I don't think we need this, we should be consistent with what we have today and everything is namespaced like http.*
    pub(crate) http: Extendable<
        ConnectorHttpInstrumentsConfig,
        Instrument<ConnectorHttpAttributes, ConnectorHttpSelector, ConnectorHttpValue>,
    >,
}
