use std::fmt::Debug;
use std::marker::PhantomData;
use std::sync::Arc;

use opentelemetry::KeyValue;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tracing::Span;
use tracing::info_span;

use super::Selector;
use super::Selectors;
use super::Stage;
use super::router::events::RouterEvents;
use super::subgraph::events::SubgraphEvents;
use super::supergraph::events::SupergraphEvents;
use crate::Context;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::connector::attributes::ConnectorAttributes;
use crate::plugins::telemetry::config_new::connector::events::ConnectorEvents;
use crate::plugins::telemetry::config_new::connector::events::ConnectorEventsConfig;
use crate::plugins::telemetry::config_new::connector::selectors::ConnectorSelector;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::router::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::router::events::RouterEventsConfig;
use crate::plugins::telemetry::config_new::router::selectors::RouterSelector;
use crate::plugins::telemetry::config_new::subgraph::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::subgraph::events::SubgraphEventsConfig;
use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphSelector;
use crate::plugins::telemetry::config_new::supergraph::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::supergraph::events::SupergraphEventsConfig;
use crate::plugins::telemetry::config_new::supergraph::selectors::SupergraphSelector;
use crate::plugins::telemetry::dynamic_attribute::EventDynAttribute;

/// Events are
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Events {
    /// Router service events
    router: Extendable<RouterEventsConfig, Event<RouterAttributes, RouterSelector>>,
    /// Subgraph service events
    supergraph: Extendable<SupergraphEventsConfig, Event<SupergraphAttributes, SupergraphSelector>>,
    /// Supergraph service events
    subgraph: Extendable<SubgraphEventsConfig, Event<SubgraphAttributes, SubgraphSelector>>,
    /// Connector events
    connector: Extendable<ConnectorEventsConfig, Event<ConnectorAttributes, ConnectorSelector>>,
}

impl Events {
    pub(crate) fn new_router_events(&self) -> RouterEvents {
        let custom_events = self
            .router
            .custom
            .iter()
            .filter_map(|(name, config)| CustomEvent::from_config(name, config))
            .collect();

        RouterEvents {
            request: StandardEvent::from_config(&self.router.attributes.request),
            response: StandardEvent::from_config(&self.router.attributes.response),
            error: StandardEvent::from_config(&self.router.attributes.error),
            custom: custom_events,
        }
    }

    pub(crate) fn new_supergraph_events(&self) -> SupergraphEvents {
        let custom_events = self
            .supergraph
            .custom
            .iter()
            .filter_map(|(name, config)| CustomEvent::from_config(name, config))
            .collect();

        SupergraphEvents {
            request: StandardEvent::from_config(&self.supergraph.attributes.request),
            response: StandardEvent::from_config(&self.supergraph.attributes.response),
            error: StandardEvent::from_config(&self.supergraph.attributes.error),
            custom: custom_events,
        }
    }

    pub(crate) fn new_subgraph_events(&self) -> SubgraphEvents {
        let custom_events = self
            .subgraph
            .custom
            .iter()
            .filter_map(|(name, config)| CustomEvent::from_config(name, config))
            .collect();

        SubgraphEvents {
            request: StandardEvent::from_config(&self.subgraph.attributes.request),
            response: StandardEvent::from_config(&self.subgraph.attributes.response),
            error: StandardEvent::from_config(&self.subgraph.attributes.error),
            custom: custom_events,
        }
    }

    pub(crate) fn new_connector_events(&self) -> ConnectorEvents {
        super::connector::events::new_connector_events(&self.connector)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        self.router
            .attributes
            .request
            .validate(Some(Stage::Request))?;
        self.router
            .attributes
            .response
            .validate(Some(Stage::Response))?;
        self.supergraph
            .attributes
            .request
            .validate(Some(Stage::Request))?;
        self.supergraph
            .attributes
            .response
            .validate(Some(Stage::Response))?;
        self.subgraph
            .attributes
            .request
            .validate(Some(Stage::Request))?;
        self.subgraph
            .attributes
            .response
            .validate(Some(Stage::Response))?;
        self.connector
            .attributes
            .request
            .validate(Some(Stage::Request))?;
        self.connector
            .attributes
            .response
            .validate(Some(Stage::Response))?;
        for (name, custom_event) in &self.router.custom {
            custom_event.validate().map_err(|err| {
                format!("configuration error for router custom event {name:?}: {err}")
            })?;
        }
        for (name, custom_event) in &self.supergraph.custom {
            custom_event.validate().map_err(|err| {
                format!("configuration error for supergraph custom event {name:?}: {err}")
            })?;
        }
        for (name, custom_event) in &self.subgraph.custom {
            custom_event.validate().map_err(|err| {
                format!("configuration error for subgraph custom event {name:?}: {err}")
            })?;
        }
        for (name, custom_event) in &self.connector.custom {
            custom_event.validate().map_err(|err| {
                format!("configuration error for connector HTTP custom event {name:?}: {err}")
            })?;
        }

        Ok(())
    }
}

pub(crate) struct CustomEvents<Request, Response, EventResponse, Attributes, Sel>
where
    Attributes: Selectors<Request, Response, EventResponse> + Default,
    Sel: Selector<Request = Request, Response = Response> + Debug,
{
    pub(super) request: Option<StandardEvent<Sel>>,
    pub(super) response: Option<StandardEvent<Sel>>,
    pub(super) error: Option<StandardEvent<Sel>>,
    pub(super) custom: Vec<CustomEvent<Request, Response, EventResponse, Attributes, Sel>>,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[schemars(rename = "StandardEventConfig{T}")]
#[serde(untagged)]
pub(crate) enum StandardEventConfig<T> {
    Level(EventLevelConfig),
    Conditional {
        level: EventLevelConfig,
        condition: Condition<T>,
    },
}

impl<T: Selector> StandardEventConfig<T> {
    fn validate(&self, restricted_stage: Option<Stage>) -> Result<(), String> {
        if let Self::Conditional { condition, .. } = self {
            condition.validate(restricted_stage)
        } else {
            Ok(())
        }
    }
}

impl<T> Default for StandardEventConfig<T> {
    fn default() -> Self {
        Self::Level(EventLevelConfig::default())
    }
}

#[derive(Debug)]
pub(crate) struct StandardEvent<T> {
    pub(crate) level: EventLevel,
    pub(crate) condition: Condition<T>,
}

impl<T: Clone> StandardEvent<T> {
    pub(crate) fn from_config(config: &StandardEventConfig<T>) -> Option<Self> {
        match &config {
            StandardEventConfig::Level(level) => EventLevel::from_config(level).map(|level| Self {
                level,
                condition: Condition::True,
            }),
            StandardEventConfig::Conditional { level, condition } => EventLevel::from_config(level)
                .map(|level| Self {
                    level,
                    condition: condition.clone(),
                }),
        }
    }
}

/// Log level configuration for events. Use "off" to not log the event, or a level name to log the
/// event at that level and above.
#[derive(Deserialize, JsonSchema, Clone, Debug, Default, PartialEq, Copy)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EventLevelConfig {
    Info,
    Warn,
    Error,
    #[default]
    Off,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub(crate) enum EventLevel {
    Info,
    Warn,
    Error,
}

impl EventLevel {
    pub(crate) fn from_config(config: &EventLevelConfig) -> Option<Self> {
        match config {
            EventLevelConfig::Off => None,
            EventLevelConfig::Info => Some(EventLevel::Info),
            EventLevelConfig::Warn => Some(EventLevel::Warn),
            EventLevelConfig::Error => Some(EventLevel::Error),
        }
    }
}

/// An event that can be logged as part of a trace.
/// The event has an implicit `type` attribute that matches the name of the event in the yaml
/// and a message that can be used to provide additional information.
#[derive(Deserialize, JsonSchema, Clone, Debug)]
pub(crate) struct Event<A, E>
where
    A: Default + Debug,
    E: Debug,
{
    /// The log level of the event.
    pub(super) level: EventLevelConfig,

    /// The event message.
    pub(super) message: Arc<String>,

    /// When to trigger the event.
    pub(super) on: EventOn,

    /// The event attributes.
    #[serde(default = "Extendable::empty_arc::<A, E>")]
    pub(super) attributes: Arc<Extendable<A, E>>,

    /// The event conditions.
    #[serde(default = "Condition::empty::<E>")]
    pub(super) condition: Condition<E>,
}

impl<A, E, Request, Response, EventResponse> Event<A, E>
where
    A: Selectors<Request, Response, EventResponse> + Default + Debug,
    E: Selector<Request = Request, Response = Response, EventResponse = EventResponse> + Debug,
{
    pub(crate) fn validate(&self) -> Result<(), String> {
        let stage = Some(self.on.into());
        self.attributes.validate(stage)?;
        self.condition.validate(stage)?;
        Ok(())
    }
}

/// When to trigger the event.
#[derive(Deserialize, JsonSchema, Clone, Debug, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EventOn {
    /// Log the event on request
    Request,
    /// Log the event on response
    Response,
    /// Log the event on every chunks in the response
    EventResponse,
    /// Log the event on error
    Error,
}

pub(crate) struct CustomEvent<Request, Response, EventResponse, A, T>
where
    A: Selectors<Request, Response, EventResponse> + Default,
    T: Selector<Request = Request, Response = Response> + Debug,
{
    pub(super) name: String,
    pub(super) level: EventLevel,
    pub(super) event_on: EventOn,
    pub(super) message: Arc<String>,
    pub(super) selectors: Arc<Extendable<A, T>>,
    pub(super) condition: Condition<T>,
    pub(super) attributes: Vec<opentelemetry::KeyValue>,
    pub(super) _phantom: PhantomData<EventResponse>,
}

impl<A, T, Request, Response, EventResponse> CustomEvent<Request, Response, EventResponse, A, T>
where
    A: Selectors<Request, Response, EventResponse> + Default + Clone + Debug,
    T: Selector<Request = Request, Response = Response, EventResponse = EventResponse>
        + Debug
        + Clone,
{
    pub(crate) fn from_config(name: &str, config: &Event<A, T>) -> Option<Self> {
        EventLevel::from_config(&config.level).map(|level| Self {
            name: name.to_owned(),
            level,
            event_on: config.on,
            message: config.message.clone(),
            selectors: config.attributes.clone(),
            condition: config.condition.clone(),
            attributes: Vec::new(),
            _phantom: PhantomData,
        })
    }

    pub(crate) fn on_request(&mut self, request: &Request) {
        if self.condition.evaluate_request(request) != Some(true)
            && self.event_on == EventOn::Request
        {
            return;
        }
        self.attributes = self.selectors.on_request(request);

        if self.event_on == EventOn::Request {
            let attrs = std::mem::take(&mut self.attributes);
            log_event(self.level, &self.name, attrs, &self.message);
        }
    }

    pub(crate) fn on_response(&mut self, response: &Response) {
        if self.event_on != EventOn::Response {
            return;
        }

        if !self.condition.evaluate_response(response) {
            return;
        }
        let mut new_attributes = self.selectors.on_response(response);
        self.attributes.append(&mut new_attributes);

        let attrs = std::mem::take(&mut self.attributes);
        log_event(self.level, &self.name, attrs, &self.message);
    }

    pub(crate) fn on_response_event(&self, response: &EventResponse, ctx: &Context) {
        if self.event_on != EventOn::EventResponse {
            return;
        }

        if !self.condition.evaluate_event_response(response, ctx) {
            return;
        }
        let mut attributes = self.attributes.clone();
        let mut new_attributes = self.selectors.on_response_event(response, ctx);
        attributes.append(&mut new_attributes);
        // Stub span to make sure the custom attributes are saved in current span extensions
        // It won't be extracted or sampled at all
        if Span::current().is_none() {
            let span = info_span!("supergraph_event_send_event");
            let _entered = span.enter();
            log_event(self.level, &self.name, attributes, &self.message);
        } else {
            log_event(self.level, &self.name, attributes, &self.message);
        }
    }

    pub(crate) fn on_error(&mut self, error: &BoxError, ctx: &Context) {
        if self.event_on != EventOn::Error {
            return;
        }
        let mut new_attributes = self.selectors.on_error(error, ctx);
        self.attributes.append(&mut new_attributes);

        let attrs = std::mem::take(&mut self.attributes);
        log_event(self.level, &self.name, attrs, &self.message);
    }
}

#[inline]
pub(crate) fn log_event(level: EventLevel, kind: &str, attributes: Vec<KeyValue>, message: &str) {
    let span = Span::current();
    #[cfg(test)]
    let mut attributes = attributes;
    #[cfg(test)]
    attributes.sort_by(|a, b| a.key.partial_cmp(&b.key).unwrap());
    span.set_event_dyn_attributes(attributes);

    match level {
        EventLevel::Info => {
            ::tracing::info!(%kind, "{}", message);
        }
        EventLevel::Warn => {
            ::tracing::warn!(%kind, "{}", message)
        }
        EventLevel::Error => {
            ::tracing::error!(%kind, "{}", message)
        }
    }
}
