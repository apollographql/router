use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::telemetry::config_new::connector::http::attributes::ConnectorHttpAttributes;
use crate::plugins::telemetry::config_new::connector::http::events::ConnectorHttpEventsConfig;
use crate::plugins::telemetry::config_new::connector::http::selectors::ConnectorHttpSelector;
use crate::plugins::telemetry::config_new::events::Event;
use crate::plugins::telemetry::config_new::extendable::Extendable;

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ConnectorEventsKind {
    // TODO: I don't think we need this, we should be consistent with what we have today and everything is namespaced like http.*
    pub(crate) http: Extendable<
        ConnectorHttpEventsConfig,
        Event<ConnectorHttpAttributes, ConnectorHttpSelector>,
    >,
}
