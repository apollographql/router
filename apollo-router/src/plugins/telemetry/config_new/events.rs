use std::fmt::Debug;
use std::marker::PhantomData;
use std::sync::Arc;

#[cfg(test)]
use http::HeaderValue;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tracing::Span;
use tracing::info_span;

use super::Selector;
use super::Selectors;
use super::Stage;
use crate::Context;
use crate::graphql;
use crate::plugins::telemetry::config_new::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::selectors::RouterSelector;
use crate::plugins::telemetry::config_new::selectors::SubgraphSelector;
use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
use crate::plugins::telemetry::dynamic_attribute::EventDynAttribute;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;

<<<<<<< HEAD
=======
#[derive(Clone)]
pub(crate) struct DisplayRouterRequest(pub(crate) EventLevel);
#[derive(Default, Clone)]
pub(crate) struct DisplayRouterResponse(pub(crate) bool);
#[derive(Default, Clone)]
pub(crate) struct RouterResponseBodyExtensionType(pub(crate) String);

>>>>>>> e7d8e7bb (Simplify implementation of telementry's events (#7280))
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

    pub(crate) fn validate(&self) -> Result<(), String> {
<<<<<<< HEAD
        if let StandardEventConfig::Conditional { condition, .. } = &self.router.attributes.request
        {
            condition.validate(Some(Stage::Request))?;
        }
        if let StandardEventConfig::Conditional { condition, .. } = &self.router.attributes.response
        {
            condition.validate(Some(Stage::Response))?;
        }
        if let StandardEventConfig::Conditional { condition, .. } =
            &self.supergraph.attributes.request
        {
            condition.validate(Some(Stage::Request))?;
        }
        if let StandardEventConfig::Conditional { condition, .. } =
            &self.supergraph.attributes.response
        {
            condition.validate(Some(Stage::Response))?;
        }
        if let StandardEventConfig::Conditional { condition, .. } =
            &self.subgraph.attributes.request
        {
            condition.validate(Some(Stage::Request))?;
        }
        if let StandardEventConfig::Conditional { condition, .. } =
            &self.subgraph.attributes.response
        {
            condition.validate(Some(Stage::Response))?;
        }
=======
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
>>>>>>> e7d8e7bb (Simplify implementation of telementry's events (#7280))
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

        Ok(())
    }
}

pub(crate) type RouterEvents =
    CustomEvents<router::Request, router::Response, (), RouterAttributes, RouterSelector>;

pub(crate) type SupergraphEvents = CustomEvents<
    supergraph::Request,
    supergraph::Response,
    graphql::Response,
    SupergraphAttributes,
    SupergraphSelector,
>;

pub(crate) type SubgraphEvents =
    CustomEvents<subgraph::Request, subgraph::Response, (), SubgraphAttributes, SubgraphSelector>;

pub(crate) struct CustomEvents<Request, Response, EventResponse, Attributes, Sel>
where
    Attributes: Selectors<Request, Response, EventResponse> + Default,
    Sel: Selector<Request = Request, Response = Response> + Debug,
{
<<<<<<< HEAD
    request: StandardEvent<Sel>,
    response: StandardEvent<Sel>,
    error: StandardEvent<Sel>,
    custom: Vec<CustomEvent<Request, Response, EventResponse, Attributes, Sel>>,
=======
    pub(super) request: Option<StandardEvent<Sel>>,
    pub(super) response: Option<StandardEvent<Sel>>,
    pub(super) error: Option<StandardEvent<Sel>>,
    pub(super) custom: Vec<CustomEvent<Request, Response, EventResponse, Attributes, Sel>>,
>>>>>>> e7d8e7bb (Simplify implementation of telementry's events (#7280))
}

impl CustomEvents<router::Request, router::Response, (), RouterAttributes, RouterSelector> {
    pub(crate) fn on_request(&mut self, request: &router::Request) {
        if let Some(request_event) = &mut self.request {
            if request_event.condition.evaluate_request(request) != Some(true) {
                return;
            }
            let mut attrs = Vec::with_capacity(5);
            #[cfg(test)]
            let mut headers: indexmap::IndexMap<String, HeaderValue> = request
                .router_request
                .headers()
                .clone()
                .into_iter()
                .filter_map(|(name, val)| Some((name?.to_string(), val)))
                .collect();
            #[cfg(test)]
            headers.sort_keys();
            #[cfg(not(test))]
            let headers = request.router_request.headers();

<<<<<<< HEAD
            attrs.push(KeyValue::new(
                Key::from_static_str("http.request.headers"),
                opentelemetry::Value::String(format!("{:?}", headers).into()),
            ));
            attrs.push(KeyValue::new(
                Key::from_static_str("http.request.method"),
                opentelemetry::Value::String(format!("{}", request.router_request.method()).into()),
            ));
            attrs.push(KeyValue::new(
                Key::from_static_str("http.request.uri"),
                opentelemetry::Value::String(format!("{}", request.router_request.uri()).into()),
            ));
            attrs.push(KeyValue::new(
                Key::from_static_str("http.request.version"),
                opentelemetry::Value::String(
                    format!("{:?}", request.router_request.version()).into(),
                ),
            ));
            attrs.push(KeyValue::new(
                Key::from_static_str("http.request.body"),
                opentelemetry::Value::String(format!("{:?}", request.router_request.body()).into()),
            ));
            log_event(self.request.level(), "router.request", attrs, "");
=======
            request
                .context
                .extensions()
                .with_lock(|ext| ext.insert(DisplayRouterRequest(request_event.level)));
        }
        if self.response.is_some() {
            request
                .context
                .extensions()
                .with_lock(|ext| ext.insert(DisplayRouterResponse(true)));
>>>>>>> e7d8e7bb (Simplify implementation of telementry's events (#7280))
        }
        for custom_event in &mut self.custom {
            custom_event.on_request(request);
        }
    }

    pub(crate) fn on_response(&mut self, response: &router::Response) {
        if let Some(response_event) = &self.response {
            if !response_event.condition.evaluate_response(response) {
                return;
            }
            let mut attrs = Vec::with_capacity(4);

            #[cfg(test)]
            let mut headers: indexmap::IndexMap<String, HeaderValue> = response
                .response
                .headers()
                .clone()
                .into_iter()
                .filter_map(|(name, val)| Some((name?.to_string(), val)))
                .collect();
            #[cfg(test)]
            headers.sort_keys();
            #[cfg(not(test))]
            let headers = response.response.headers();
            attrs.push(KeyValue::new(
                Key::from_static_str("http.response.headers"),
                opentelemetry::Value::String(format!("{:?}", headers).into()),
            ));
            attrs.push(KeyValue::new(
                Key::from_static_str("http.response.status"),
                opentelemetry::Value::String(format!("{}", response.response.status()).into()),
            ));
            attrs.push(KeyValue::new(
                Key::from_static_str("http.response.version"),
                opentelemetry::Value::String(format!("{:?}", response.response.version()).into()),
            ));
<<<<<<< HEAD
            attrs.push(KeyValue::new(
                Key::from_static_str("http.response.body"),
                opentelemetry::Value::String(format!("{:?}", response.response.body()).into()),
            ));
            log_event(self.response.level(), "router.response", attrs, "");
=======

            if let Some(body) = response
                .context
                .extensions()
                .with_lock(|ext| ext.remove::<RouterResponseBodyExtensionType>())
            {
                attrs.push(KeyValue::new(
                    HTTP_RESPONSE_BODY,
                    opentelemetry::Value::String(body.0.into()),
                ));
            }

            log_event(response_event.level, "router.response", attrs, "");
>>>>>>> e7d8e7bb (Simplify implementation of telementry's events (#7280))
        }
        for custom_event in &mut self.custom {
            custom_event.on_response(response);
        }
    }

    pub(crate) fn on_error(&mut self, error: &BoxError, ctx: &Context) {
        if let Some(error_event) = &self.error {
            if !error_event.condition.evaluate_error(error, ctx) {
                return;
            }
            log_event(
                error_event.level,
                "router.error",
                vec![KeyValue::new(
                    Key::from_static_str("error"),
                    opentelemetry::Value::String(error.to_string().into()),
                )],
                "",
            );
        }
        for custom_event in &mut self.custom {
            custom_event.on_error(error, ctx);
        }
    }
}

impl
    CustomEvents<
        supergraph::Request,
        supergraph::Response,
        graphql::Response,
        SupergraphAttributes,
        SupergraphSelector,
    >
{
    pub(crate) fn on_request(&mut self, request: &supergraph::Request) {
        if let Some(request_event) = &mut self.request {
            if request_event.condition.evaluate_request(request) != Some(true) {
                return;
            }
            let mut attrs = Vec::with_capacity(5);
            #[cfg(test)]
            let mut headers: indexmap::IndexMap<String, HeaderValue> = request
                .supergraph_request
                .headers()
                .clone()
                .into_iter()
                .filter_map(|(name, val)| Some((name?.to_string(), val)))
                .collect();
            #[cfg(test)]
            headers.sort_keys();
            #[cfg(not(test))]
            let headers = request.supergraph_request.headers();
            attrs.push(KeyValue::new(
                Key::from_static_str("http.request.headers"),
                opentelemetry::Value::String(format!("{:?}", headers).into()),
            ));
            attrs.push(KeyValue::new(
                Key::from_static_str("http.request.method"),
                opentelemetry::Value::String(
                    format!("{}", request.supergraph_request.method()).into(),
                ),
            ));
            attrs.push(KeyValue::new(
                Key::from_static_str("http.request.uri"),
                opentelemetry::Value::String(
                    format!("{}", request.supergraph_request.uri()).into(),
                ),
            ));
            attrs.push(KeyValue::new(
                Key::from_static_str("http.request.version"),
                opentelemetry::Value::String(
                    format!("{:?}", request.supergraph_request.version()).into(),
                ),
            ));
            attrs.push(KeyValue::new(
                Key::from_static_str("http.request.body"),
                opentelemetry::Value::String(
                    serde_json::to_string(request.supergraph_request.body())
                        .unwrap_or_default()
                        .into(),
                ),
            ));
            log_event(request_event.level, "supergraph.request", attrs, "");
        }
<<<<<<< HEAD
        if self.response.level() != EventLevel::Off {
            request
                .context
                .extensions()
                .with_lock(|mut lock| lock.insert(SupergraphEventResponse(self.response.clone())));
=======
        if let Some(response_event) = self.response.take() {
            request.context.extensions().with_lock(|lock| {
                lock.insert(SupergraphEventResponse {
                    level: response_event.level,
                    condition: Arc::new(response_event.condition),
                })
            });
>>>>>>> e7d8e7bb (Simplify implementation of telementry's events (#7280))
        }
        for custom_event in &mut self.custom {
            custom_event.on_request(request);
        }
    }

    pub(crate) fn on_response(&mut self, response: &supergraph::Response) {
        for custom_event in &mut self.custom {
            custom_event.on_response(response);
        }
    }

    pub(crate) fn on_response_event(&self, response: &graphql::Response, ctx: &Context) {
        for custom_event in &self.custom {
            custom_event.on_response_event(response, ctx);
        }
    }

    pub(crate) fn on_error(&mut self, error: &BoxError, ctx: &Context) {
        if let Some(error_event) = &self.error {
            if !error_event.condition.evaluate_error(error, ctx) {
                return;
            }
            log_event(
                error_event.level,
                "supergraph.error",
                vec![KeyValue::new(
                    Key::from_static_str("error"),
                    opentelemetry::Value::String(error.to_string().into()),
                )],
                "",
            );
        }
        for custom_event in &mut self.custom {
            custom_event.on_error(error, ctx);
        }
    }
}

impl CustomEvents<subgraph::Request, subgraph::Response, (), SubgraphAttributes, SubgraphSelector> {
    pub(crate) fn on_request(&mut self, request: &subgraph::Request) {
        if let Some(mut request_event) = self.request.take() {
            if request_event.condition.evaluate_request(request) != Some(true) {
                return;
            }
            request.context.extensions().with_lock(|lock| {
                lock.insert(SubgraphEventRequest {
                    level: request_event.level,
                    condition: Arc::new(Mutex::new(request_event.condition)),
                })
            });
        }
<<<<<<< HEAD
        if self.request.level() != EventLevel::Off {
            request
                .context
                .extensions()
                .with_lock(|mut lock| lock.insert(SubgraphEventRequest(self.request.clone())));
        }
        if self.response.level() != EventLevel::Off {
            request
                .context
                .extensions()
                .with_lock(|mut lock| lock.insert(SubgraphEventResponse(self.response.clone())));
        }
        for custom_event in &self.custom {
=======
        if let Some(response_event) = self.response.take() {
            request.context.extensions().with_lock(|lock| {
                lock.insert(SubgraphEventResponse {
                    level: response_event.level,
                    condition: Arc::new(response_event.condition),
                })
            });
        }
        for custom_event in &mut self.custom {
>>>>>>> e7d8e7bb (Simplify implementation of telementry's events (#7280))
            custom_event.on_request(request);
        }
    }

    pub(crate) fn on_response(&mut self, response: &subgraph::Response) {
        for custom_event in &mut self.custom {
            custom_event.on_response(response);
        }
    }

    pub(crate) fn on_error(&mut self, error: &BoxError, ctx: &Context) {
        if let Some(error_event) = &self.error {
            if !error_event.condition.evaluate_error(error, ctx) {
                return;
            }
            log_event(
                error_event.level,
                "subgraph.error",
                vec![KeyValue::new(
                    Key::from_static_str("error"),
                    opentelemetry::Value::String(error.to_string().into()),
                )],
                "",
            );
        }
        for custom_event in &mut self.custom {
            custom_event.on_error(error, ctx);
        }
    }
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
struct RouterEventsConfig {
    /// Log the router request
    request: StandardEventConfig<RouterSelector>,
    /// Log the router response
    response: StandardEventConfig<RouterSelector>,
    /// Log the router error
    error: StandardEventConfig<RouterSelector>,
}

#[derive(Clone)]
pub(crate) struct SupergraphEventResponse {
    // XXX(@IvanGoncharov): As part of removing Arc from StandardEvent I moved it here
    // I think it's not nessary here but can't verify it right now, so in future can just wrap StandardEvent
    pub(crate) level: EventLevel,
    pub(crate) condition: Arc<Condition<SupergraphSelector>>,
}

#[derive(Clone)]
pub(crate) struct SubgraphEventResponse {
    // XXX(@IvanGoncharov): As part of removing Arc from StandardEvent I moved it here
    // I think it's not nessary here but can't verify it right now, so in future can just wrap StandardEvent
    pub(crate) level: EventLevel,
    pub(crate) condition: Arc<Condition<SubgraphSelector>>,
}

#[derive(Clone)]
pub(crate) struct SubgraphEventRequest {
    // XXX(@IvanGoncharov): As part of removing Mutex from StandardEvent I moved it here
    // I think it's not nessary here but can't verify it right now, so in future can just wrap StandardEvent
    pub(crate) level: EventLevel,
    pub(crate) condition: Arc<Mutex<Condition<SubgraphSelector>>>,
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
struct SupergraphEventsConfig {
    /// Log the supergraph request
    request: StandardEventConfig<SupergraphSelector>,
    /// Log the supergraph response
    response: StandardEventConfig<SupergraphSelector>,
    /// Log the supergraph error
    error: StandardEventConfig<SupergraphSelector>,
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
struct SubgraphEventsConfig {
    /// Log the subgraph request
    request: StandardEventConfig<SubgraphSelector>,
    /// Log the subgraph response
    response: StandardEventConfig<SubgraphSelector>,
    /// Log the subgraph error
    error: StandardEventConfig<SubgraphSelector>,
}

#[derive(Deserialize, JsonSchema, Clone, Debug)]
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
<<<<<<< HEAD
    level: EventLevel,
=======
    pub(super) level: EventLevelConfig,
>>>>>>> e7d8e7bb (Simplify implementation of telementry's events (#7280))

    /// The event message.
    message: Arc<String>,

    /// When to trigger the event.
    on: EventOn,

    /// The event attributes.
    #[serde(default = "Extendable::empty_arc::<A, E>")]
    attributes: Arc<Extendable<A, E>>,

    /// The event conditions.
    #[serde(default = "Condition::empty::<E>")]
    condition: Condition<E>,
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
<<<<<<< HEAD
    inner: Mutex<CustomEventInner<Request, Response, EventResponse, A, T>>,
}

struct CustomEventInner<Request, Response, EventResponse, A, T>
where
    A: Selectors<Request, Response, EventResponse> + Default,
    T: Selector<Request = Request, Response = Response> + Debug,
{
    name: String,
    level: EventLevel,
    event_on: EventOn,
    message: Arc<String>,
    selectors: Option<Arc<Extendable<A, T>>>,
    condition: Condition<T>,
    attributes: Vec<opentelemetry_api::KeyValue>,
    _phantom: PhantomData<EventResponse>,
=======
    pub(super) name: String,
    pub(super) level: EventLevel,
    pub(super) event_on: EventOn,
    pub(super) message: Arc<String>,
    pub(super) selectors: Arc<Extendable<A, T>>,
    pub(super) condition: Condition<T>,
    pub(super) attributes: Vec<opentelemetry::KeyValue>,
    pub(super) _phantom: PhantomData<EventResponse>,
>>>>>>> e7d8e7bb (Simplify implementation of telementry's events (#7280))
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

        if self.event_on == EventOn::Request
            && self.condition.evaluate_request(request) != Some(false)
        {
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

#[cfg(test)]
mod tests {
    use http::HeaderValue;
    use http::header::CONTENT_LENGTH;
    use tracing::instrument::WithSubscriber;

    use super::*;
    use crate::assert_snapshot_subscriber;
    use crate::context::CONTAINS_GRAPHQL_ERROR;
    use crate::context::OPERATION_NAME;
    use crate::graphql;
    use crate::plugins::telemetry::Telemetry;
    use crate::plugins::test::PluginTestHarness;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_router_events() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../testdata/custom_events.router.yaml"))
            .build()
            .await;

        async {
            test_harness
                .call_router(
                    router::Request::fake_builder()
                        .header(CONTENT_LENGTH, "0")
                        .header("custom-header", "val1")
                        .header("x-log-request", HeaderValue::from_static("log"))
                        .build()
                        .unwrap(),
                    |_r|async  {
                        Ok(router::Response::fake_builder()
                            .header("custom-header", "val1")
                            .header(CONTENT_LENGTH, "25")
                            .header("x-log-request", HeaderValue::from_static("log"))
                            .data(serde_json_bytes::json!({"data": "res"}))
                            .build()
                            .expect("expecting valid response"))
                    },
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(
            assert_snapshot_subscriber!({r#"[].span["apollo_private.duration_ns"]"# => "[duration]", r#"[].spans[]["apollo_private.duration_ns"]"# => "[duration]", "[].fields.attributes" => insta::sorted_redaction()}),
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_router_events_graphql_error() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../testdata/custom_events.router.yaml"))
            .build()
            .await;

        async {
            // Without the header to enable custom event
            test_harness
                .call_router(
                    router::Request::fake_builder()
                        .header("custom-header", "val1")
                        .build()
                        .unwrap(),
                    |_r| async {
                        let context_with_error = Context::new();
                        let _ = context_with_error
                            .insert(CONTAINS_GRAPHQL_ERROR, true)
                            .unwrap();
                        Ok(router::Response::fake_builder()
                            .header("custom-header", "val1")
                            .context(context_with_error)
                            .data(serde_json_bytes::json!({"errors": [{"message": "res"}]}))
                            .build()
                            .expect("expecting valid response"))
                    },
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(
            assert_snapshot_subscriber!({r#"[].span["apollo_private.duration_ns"]"# => "[duration]", r#"[].spans[]["apollo_private.duration_ns"]"# => "[duration]", "[].fields.attributes" => insta::sorted_redaction()}),
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_router_events_graphql_response() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../testdata/custom_events.router.yaml"))
            .build()
            .await;

        async {
            // Without the header to enable custom event
            test_harness
                .call_router(
                    router::Request::fake_builder()
                        .header("custom-header", "val1")
                        .build()
                        .unwrap(),
                    |_r| async {
                        Ok(router::Response::fake_builder()
                            .header("custom-header", "val1")
                            .header(CONTENT_LENGTH, "25")
                            .header("x-log-response", HeaderValue::from_static("log"))
                            .data(serde_json_bytes::json!({"data": "res"}))
                            .build()
                            .expect("expecting valid response"))
                    },
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(
            assert_snapshot_subscriber!({r#"[].span["apollo_private.duration_ns"]"# => "[duration]", r#"[].spans[]["apollo_private.duration_ns"]"# => "[duration]", "[].fields.attributes" => insta::sorted_redaction()}),
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_supergraph_events() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../testdata/custom_events.router.yaml"))
            .build()
            .await;

        async {
            test_harness
                .call_supergraph(
                    supergraph::Request::fake_builder()
                        .query("query { foo }")
                        .header("x-log-request", HeaderValue::from_static("log"))
                        .build()
                        .unwrap(),
                    |_r| async {
                        supergraph::Response::fake_builder()
                            .header("custom-header", "val1")
                            .header("x-log-request", HeaderValue::from_static("log"))
                            .data(serde_json::json!({"data": "res"}).to_string())
                            .build()
                    },
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_supergraph_events_with_exists_condition() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!(
                "../testdata/custom_events_exists_condition.router.yaml"
            ))
            .build()
            .await;

        async {
            let ctx = Context::new();
            ctx.insert(OPERATION_NAME, String::from("Test")).unwrap();
            test_harness
                .call_supergraph(
                    supergraph::Request::fake_builder()
                        .query("query Test { foo }")
                        .context(ctx)
                        .build()
                        .unwrap(),
                    |_r| async {
                        supergraph::Response::fake_builder()
                            .data(serde_json::json!({"data": "res"}).to_string())
                            .build()
                    },
                )
                .await
                .expect("expecting successful response");
            test_harness
                .call_supergraph(
                    supergraph::Request::fake_builder()
                        .query("query { foo }")
                        .build()
                        .unwrap(),
                    |_r| async {
                        supergraph::Response::fake_builder()
                            .data(serde_json::json!({"data": "res"}).to_string())
                            .build()
                    },
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_supergraph_events_on_graphql_error() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../testdata/custom_events.router.yaml"))
            .build()
            .await;

        async {
            test_harness
                .call_supergraph(
                    supergraph::Request::fake_builder()
                        .query("query { foo }")
                        .build()
                        .unwrap(),
                    |_r| async {
                        let context_with_error = Context::new();
                        let _ = context_with_error
                            .insert(CONTAINS_GRAPHQL_ERROR, true)
                            .unwrap();
                        supergraph::Response::fake_builder()
                            .header("custom-header", "val1")
                            .header("x-log-request", HeaderValue::from_static("log"))
                            .context(context_with_error)
                            .data(serde_json_bytes::json!({"errors": [{"message": "res"}]}))
                            .build()
                    },
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_supergraph_events_on_response() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../testdata/custom_events.router.yaml"))
            .build()
            .await;

        async {
            test_harness
                .call_supergraph(
                    supergraph::Request::fake_builder()
                        .query("query { foo }")
                        .build()
                        .unwrap(),
                    |_r| async {
                        supergraph::Response::fake_builder()
                            .header("custom-header", "val1")
                            .header("x-log-response", HeaderValue::from_static("log"))
                            .data(serde_json_bytes::json!({"errors": [{"message": "res"}]}))
                            .build()
                    },
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_events() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../testdata/custom_events.router.yaml"))
            .build()
            .await;

        async {
            let mut subgraph_req = http::Request::new(
                graphql::Request::fake_builder()
                    .query("query { foo }")
                    .build(),
            );
            subgraph_req
                .headers_mut()
                .insert("x-log-request", HeaderValue::from_static("log"));
            test_harness
                .call_subgraph(
                    subgraph::Request::fake_builder()
                        .subgraph_name("subgraph")
                        .subgraph_request(subgraph_req)
                        .build(),
                    |_r| async {
                        subgraph::Response::fake2_builder()
                            .header("custom-header", "val1")
                            .header("x-log-request", HeaderValue::from_static("log"))
                            .data(serde_json::json!({"data": "res"}).to_string())
                            .build()
                    },
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_events_response() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../testdata/custom_events.router.yaml"))
            .build()
            .await;

        async {
            let mut subgraph_req = http::Request::new(
                graphql::Request::fake_builder()
                    .query("query { foo }")
                    .build(),
            );
            subgraph_req
                .headers_mut()
                .insert("x-log-request", HeaderValue::from_static("log"));
            test_harness
                .call_subgraph(
                    subgraph::Request::fake_builder()
                        .subgraph_name("subgraph")
                        .subgraph_request(subgraph_req)
                        .build(),
                    |_r| async {
                        subgraph::Response::fake2_builder()
                            .header("custom-header", "val1")
                            .header("x-log-response", HeaderValue::from_static("log"))
                            .subgraph_name("subgraph")
                            .data(serde_json::json!({"data": "res"}).to_string())
                            .build()
                    },
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }
}
