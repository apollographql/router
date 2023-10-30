use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::telemetry::config_new::attributes::Extendable;
use crate::plugins::telemetry::config_new::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::attributes::RouterCustomAttribute;
use crate::plugins::telemetry::config_new::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::attributes::SubgraphCustomAttribute;
use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::attributes::SupergraphCustomAttribute;

/// Events are
#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Events {
    /// Router service events
    router: Extendable<RouterEvents, Event<RouterAttributes, RouterCustomAttribute>>,
    /// Subgraph service events
    supergraph:
        Extendable<SupergraphEvents, Event<SupergraphAttributes, SupergraphCustomAttribute>>,
    /// Supergraph service events
    subgraph: Extendable<SubgraphEvents, Event<SubgraphAttributes, SubgraphCustomAttribute>>,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
struct RouterEvents {
    /// Log the router request
    request: bool,
    /// Log the router response
    response: bool,
    /// Log the router error
    error: bool,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
struct SupergraphEvents {
    /// Log the supergraph request
    request: EventLevel,
    /// Log the supergraph response
    response: EventLevel,
    /// Log the supergraph error
    error: EventLevel,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
struct SubgraphEvents {
    /// Log the subgraph request
    request: EventLevel,
    /// Log the subgraph response
    response: EventLevel,
    /// Log the subgraph error
    error: EventLevel,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EventLevel {
    Info,
    Warn,
    Error,
    #[default]
    Off,
}

/// An event that can be logged as part of a trace.
/// The event has an implicit `type` attribute that matches the name of the event in the yaml
/// and a message that can be used to provide additional information.
#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
pub(crate) struct Event<A, E>
where
    A: Default,
{
    /// The log level of the event.
    level: EventLevel,
    /// The event message.
    message: String,
    /// The event attributes.
    #[serde(default = "Extendable::empty::<A, E>")]
    attributes: Extendable<A, E>,
}
