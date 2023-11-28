use std::fmt::Debug;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::telemetry::config_new::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::selectors::RouterSelector;
use crate::plugins::telemetry::config_new::selectors::SubgraphSelector;
use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;

/// Events are
#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Events {
    /// Router service events
    router: Extendable<RouterEvents, Event<RouterAttributes, RouterSelector>>,
    /// Subgraph service events
    supergraph: Extendable<SupergraphEvents, Event<SupergraphAttributes, SupergraphSelector>>,
    /// Supergraph service events
    subgraph: Extendable<SubgraphEvents, Event<SubgraphAttributes, SubgraphSelector>>,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
struct RouterEvents {
    /// Log the router request
    request: EventLevel,
    /// Log the router response
    response: EventLevel,
    /// Log the router error
    error: EventLevel,
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
    A: Default + Debug,
    E: Debug,
{
    /// The log level of the event.
    level: EventLevel,

    /// The event message.
    message: String,

    /// When to trigger the event.
    on: EventOn,

    /// The event attributes.
    #[serde(default = "Extendable::empty::<A, E>")]
    attributes: Extendable<A, E>,

    /// The event conditions.
    #[serde(default = "Condition::empty::<E>")]
    condition: Condition<E>,
}

/// When to trigger the event.
#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EventOn {
    /// Log the event on request
    Request,
    /// Log the event on response
    Response,
    /// Log the event on error
    Error,
}
