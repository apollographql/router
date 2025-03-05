use std::fmt::Debug;
use std::marker::PhantomData;
use std::sync::Arc;

#[cfg(test)]
use http::HeaderValue;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tracing::Span;
use tracing::info_span;

use super::Selector;
use super::Selectors;
use super::Stage;
use super::instruments::Instrumented;
use crate::Context;
use crate::graphql;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_BODY;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_HEADERS;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_URI;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_VERSION;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_BODY;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_HEADERS;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_STATUS;
use crate::plugins::telemetry::config_new::attributes::HTTP_RESPONSE_VERSION;
use crate::plugins::telemetry::config_new::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::connector::attributes::ConnectorAttributes;
use crate::plugins::telemetry::config_new::connector::events::ConnectorEvents;
use crate::plugins::telemetry::config_new::connector::events::ConnectorEventsConfig;
use crate::plugins::telemetry::config_new::connector::selectors::ConnectorSelector;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::selectors::RouterSelector;
use crate::plugins::telemetry::config_new::selectors::SubgraphSelector;
use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
use crate::plugins::telemetry::dynamic_attribute::EventDynAttribute;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;

#[derive(Default, Clone)]
pub(crate) struct DisplayRouterRequest(pub(crate) EventLevel);
#[derive(Default, Clone)]
pub(crate) struct DisplayRouterResponse(pub(crate) bool);
#[derive(Default, Clone)]
pub(crate) struct RouterResponseBodyExtensionType(pub(crate) String);

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
            .filter_map(|(event_name, event_cfg)| match &event_cfg.level {
                EventLevel::Off => None,
                _ => Some(CustomEvent {
                    inner: Mutex::new(CustomEventInner {
                        name: event_name.clone(),
                        level: event_cfg.level,
                        event_on: event_cfg.on,
                        message: event_cfg.message.clone(),
                        selectors: event_cfg.attributes.clone().into(),
                        condition: event_cfg.condition.clone(),
                        attributes: Vec::new(),
                        _phantom: PhantomData,
                    }),
                }),
            })
            .collect();

        RouterEvents {
            request: self.router.attributes.request.clone().into(),
            response: self.router.attributes.response.clone().into(),
            error: self.router.attributes.error.clone().into(),
            custom: custom_events,
        }
    }

    pub(crate) fn new_supergraph_events(&self) -> SupergraphEvents {
        let custom_events = self
            .supergraph
            .custom
            .iter()
            .filter_map(|(event_name, event_cfg)| match &event_cfg.level {
                EventLevel::Off => None,
                _ => Some(CustomEvent {
                    inner: Mutex::new(CustomEventInner {
                        name: event_name.clone(),
                        level: event_cfg.level,
                        event_on: event_cfg.on,
                        message: event_cfg.message.clone(),
                        selectors: event_cfg.attributes.clone().into(),
                        condition: event_cfg.condition.clone(),
                        attributes: Vec::new(),
                        _phantom: PhantomData,
                    }),
                }),
            })
            .collect();

        SupergraphEvents {
            request: self.supergraph.attributes.request.clone().into(),
            response: self.supergraph.attributes.response.clone().into(),
            error: self.supergraph.attributes.error.clone().into(),
            custom: custom_events,
        }
    }

    pub(crate) fn new_subgraph_events(&self) -> SubgraphEvents {
        let custom_events = self
            .subgraph
            .custom
            .iter()
            .filter_map(|(event_name, event_cfg)| match &event_cfg.level {
                EventLevel::Off => None,
                _ => Some(CustomEvent {
                    inner: Mutex::new(CustomEventInner {
                        name: event_name.clone(),
                        level: event_cfg.level,
                        event_on: event_cfg.on,
                        message: event_cfg.message.clone(),
                        selectors: event_cfg.attributes.clone().into(),
                        condition: event_cfg.condition.clone(),
                        attributes: Vec::new(),
                        _phantom: PhantomData,
                    }),
                }),
            })
            .collect();

        SubgraphEvents {
            request: self.subgraph.attributes.request.clone().into(),
            response: self.subgraph.attributes.response.clone().into(),
            error: self.subgraph.attributes.error.clone().into(),
            custom: custom_events,
        }
    }

    pub(crate) fn new_connector_events(&self) -> ConnectorEvents {
        super::connector::events::new_connector_events(&self.connector)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
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
        if let StandardEventConfig::Conditional { condition, .. } =
            &self.connector.attributes.request
        {
            condition.validate(Some(Stage::Request))?;
        }
        if let StandardEventConfig::Conditional { condition, .. } =
            &self.connector.attributes.response
        {
            condition.validate(Some(Stage::Response))?;
        }
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
    pub(super) request: StandardEvent<Sel>,
    pub(super) response: StandardEvent<Sel>,
    pub(super) error: StandardEvent<Sel>,
    pub(super) custom: Vec<CustomEvent<Request, Response, EventResponse, Attributes, Sel>>,
}

impl Instrumented
    for CustomEvents<router::Request, router::Response, (), RouterAttributes, RouterSelector>
{
    type Request = router::Request;
    type Response = router::Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        if self.request.level() != EventLevel::Off {
            if let Some(condition) = self.request.condition() {
                if condition.lock().evaluate_request(request) != Some(true) {
                    return;
                }
            }

            request
                .context
                .extensions()
                .with_lock(|ext| ext.insert(DisplayRouterRequest(self.request.level())));
        }
        if self.response.level() != EventLevel::Off {
            request
                .context
                .extensions()
                .with_lock(|ext| ext.insert(DisplayRouterResponse(true)));
        }
        for custom_event in &self.custom {
            custom_event.on_request(request);
        }
    }

    fn on_response(&self, response: &Self::Response) {
        if self.response.level() != EventLevel::Off {
            if let Some(condition) = self.response.condition() {
                if !condition.lock().evaluate_response(response) {
                    return;
                }
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
                HTTP_RESPONSE_HEADERS,
                opentelemetry::Value::String(format!("{:?}", headers).into()),
            ));
            attrs.push(KeyValue::new(
                HTTP_RESPONSE_STATUS,
                opentelemetry::Value::String(format!("{}", response.response.status()).into()),
            ));
            attrs.push(KeyValue::new(
                HTTP_RESPONSE_VERSION,
                opentelemetry::Value::String(format!("{:?}", response.response.version()).into()),
            ));

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

            log_event(self.response.level(), "router.response", attrs, "");
        }
        for custom_event in &self.custom {
            custom_event.on_response(response);
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        if self.error.level() != EventLevel::Off {
            if let Some(condition) = self.error.condition() {
                if !condition.lock().evaluate_error(error, ctx) {
                    return;
                }
            }
            log_event(
                self.error.level(),
                "router.error",
                vec![KeyValue::new(
                    Key::from_static_str("error"),
                    opentelemetry::Value::String(error.to_string().into()),
                )],
                "",
            );
        }
        for custom_event in &self.custom {
            custom_event.on_error(error, ctx);
        }
    }
}

impl Instrumented
    for CustomEvents<
        supergraph::Request,
        supergraph::Response,
        crate::graphql::Response,
        SupergraphAttributes,
        SupergraphSelector,
    >
{
    type Request = supergraph::Request;
    type Response = supergraph::Response;
    type EventResponse = crate::graphql::Response;

    fn on_request(&self, request: &Self::Request) {
        if self.request.level() != EventLevel::Off {
            if let Some(condition) = self.request.condition() {
                if condition.lock().evaluate_request(request) != Some(true) {
                    return;
                }
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
                HTTP_REQUEST_HEADERS,
                opentelemetry::Value::String(format!("{:?}", headers).into()),
            ));
            attrs.push(KeyValue::new(
                HTTP_REQUEST_METHOD,
                opentelemetry::Value::String(
                    format!("{}", request.supergraph_request.method()).into(),
                ),
            ));
            attrs.push(KeyValue::new(
                HTTP_REQUEST_URI,
                opentelemetry::Value::String(
                    format!("{}", request.supergraph_request.uri()).into(),
                ),
            ));
            attrs.push(KeyValue::new(
                HTTP_REQUEST_VERSION,
                opentelemetry::Value::String(
                    format!("{:?}", request.supergraph_request.version()).into(),
                ),
            ));
            attrs.push(KeyValue::new(
                HTTP_REQUEST_BODY,
                opentelemetry::Value::String(
                    serde_json::to_string(request.supergraph_request.body())
                        .unwrap_or_default()
                        .into(),
                ),
            ));
            log_event(self.request.level(), "supergraph.request", attrs, "");
        }
        if self.response.level() != EventLevel::Off {
            request
                .context
                .extensions()
                .with_lock(|lock| lock.insert(SupergraphEventResponse(self.response.clone())));
        }
        for custom_event in &self.custom {
            custom_event.on_request(request);
        }
    }

    fn on_response(&self, response: &Self::Response) {
        for custom_event in &self.custom {
            custom_event.on_response(response);
        }
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        for custom_event in &self.custom {
            custom_event.on_response_event(response, ctx);
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        if self.error.level() != EventLevel::Off {
            if let Some(condition) = self.error.condition() {
                if !condition.lock().evaluate_error(error, ctx) {
                    return;
                }
            }
            log_event(
                self.error.level(),
                "supergraph.error",
                vec![KeyValue::new(
                    Key::from_static_str("error"),
                    opentelemetry::Value::String(error.to_string().into()),
                )],
                "",
            );
        }
        for custom_event in &self.custom {
            custom_event.on_error(error, ctx);
        }
    }
}

impl Instrumented
    for CustomEvents<
        subgraph::Request,
        subgraph::Response,
        (),
        SubgraphAttributes,
        SubgraphSelector,
    >
{
    type Request = subgraph::Request;
    type Response = subgraph::Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        if let Some(condition) = self.request.condition() {
            if condition.lock().evaluate_request(request) != Some(true) {
                return;
            }
        }
        if self.request.level() != EventLevel::Off {
            request
                .context
                .extensions()
                .with_lock(|lock| lock.insert(SubgraphEventRequest(self.request.clone())));
        }
        if self.response.level() != EventLevel::Off {
            request
                .context
                .extensions()
                .with_lock(|lock| lock.insert(SubgraphEventResponse(self.response.clone())));
        }
        for custom_event in &self.custom {
            custom_event.on_request(request);
        }
    }

    fn on_response(&self, response: &Self::Response) {
        for custom_event in &self.custom {
            custom_event.on_response(response);
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        if self.error.level() != EventLevel::Off {
            if let Some(condition) = self.error.condition() {
                if !condition.lock().evaluate_error(error, ctx) {
                    return;
                }
            }
            log_event(
                self.error.level(),
                "subgraph.error",
                vec![KeyValue::new(
                    Key::from_static_str("error"),
                    opentelemetry::Value::String(error.to_string().into()),
                )],
                "",
            );
        }
        for custom_event in &self.custom {
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
pub(crate) struct SupergraphEventResponse(pub(crate) StandardEvent<SupergraphSelector>);
#[derive(Clone)]
pub(crate) struct SubgraphEventResponse(pub(crate) StandardEvent<SubgraphSelector>);
#[derive(Clone)]
pub(crate) struct SubgraphEventRequest(pub(crate) StandardEvent<SubgraphSelector>);

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
    Level(EventLevel),
    Conditional {
        level: EventLevel,
        condition: Condition<T>,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum StandardEvent<T> {
    Level(EventLevel),
    Conditional {
        level: EventLevel,
        condition: Arc<Mutex<Condition<T>>>,
    },
}

impl<T> From<StandardEventConfig<T>> for StandardEvent<T> {
    fn from(value: StandardEventConfig<T>) -> Self {
        match value {
            StandardEventConfig::Level(level) => StandardEvent::Level(level),
            StandardEventConfig::Conditional { level, condition } => StandardEvent::Conditional {
                level,
                condition: Arc::new(Mutex::new(condition)),
            },
        }
    }
}

impl<T> Default for StandardEventConfig<T> {
    fn default() -> Self {
        Self::Level(EventLevel::default())
    }
}

impl<T> StandardEvent<T> {
    pub(crate) fn level(&self) -> EventLevel {
        match self {
            Self::Level(level) => *level,
            Self::Conditional { level, .. } => *level,
        }
    }

    pub(crate) fn condition(&self) -> Option<&Arc<Mutex<Condition<T>>>> {
        match self {
            Self::Level(_) => None,
            Self::Conditional { condition, .. } => Some(condition),
        }
    }
}

#[derive(Deserialize, JsonSchema, Clone, Debug, Default, PartialEq, Copy)]
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
#[derive(Deserialize, JsonSchema, Clone, Debug)]
pub(crate) struct Event<A, E>
where
    A: Default + Debug,
    E: Debug,
{
    /// The log level of the event.
    pub(super) level: EventLevel,

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
    pub(super) inner: Mutex<CustomEventInner<Request, Response, EventResponse, A, T>>,
}

pub(super) struct CustomEventInner<Request, Response, EventResponse, A, T>
where
    A: Selectors<Request, Response, EventResponse> + Default,
    T: Selector<Request = Request, Response = Response> + Debug,
{
    pub(super) name: String,
    pub(super) level: EventLevel,
    pub(super) event_on: EventOn,
    pub(super) message: Arc<String>,
    pub(super) selectors: Option<Arc<Extendable<A, T>>>,
    pub(super) condition: Condition<T>,
    pub(super) attributes: Vec<opentelemetry::KeyValue>,
    pub(super) _phantom: PhantomData<EventResponse>,
}

impl<A, T, Request, Response, EventResponse> Instrumented
    for CustomEvent<Request, Response, EventResponse, A, T>
where
    A: Selectors<Request, Response, EventResponse> + Default,
    T: Selector<Request = Request, Response = Response, EventResponse = EventResponse>
        + Debug
        + Debug,
{
    type Request = Request;
    type Response = Response;
    type EventResponse = EventResponse;

    fn on_request(&self, request: &Self::Request) {
        let mut inner = self.inner.lock();
        if inner.condition.evaluate_request(request) != Some(true)
            && inner.event_on == EventOn::Request
        {
            return;
        }
        if let Some(selectors) = &inner.selectors {
            inner.attributes = selectors.on_request(request);
        }

        if inner.event_on == EventOn::Request
            && inner.condition.evaluate_request(request) != Some(false)
        {
            let attrs = std::mem::take(&mut inner.attributes);
            inner.send_event(attrs);
        }
    }

    fn on_response(&self, response: &Self::Response) {
        let mut inner = self.inner.lock();
        if inner.event_on != EventOn::Response {
            return;
        }

        if !inner.condition.evaluate_response(response) {
            return;
        }
        if let Some(selectors) = &inner.selectors {
            let mut new_attributes = selectors.on_response(response);
            inner.attributes.append(&mut new_attributes);
        }

        let attrs = std::mem::take(&mut inner.attributes);
        inner.send_event(attrs);
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        let inner = self.inner.lock();
        if inner.event_on != EventOn::EventResponse {
            return;
        }

        if !inner.condition.evaluate_event_response(response, ctx) {
            return;
        }
        let mut attributes = inner.attributes.clone();
        if let Some(selectors) = &inner.selectors {
            let mut new_attributes = selectors.on_response_event(response, ctx);
            attributes.append(&mut new_attributes);
        }
        // Stub span to make sure the custom attributes are saved in current span extensions
        // It won't be extracted or sampled at all
        if Span::current().is_none() {
            let span = info_span!("supergraph_event_send_event");
            let _entered = span.enter();
            inner.send_event(attributes);
        } else {
            inner.send_event(attributes);
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        let mut inner = self.inner.lock();
        if inner.event_on != EventOn::Error {
            return;
        }
        if let Some(selectors) = &inner.selectors {
            let mut new_attributes = selectors.on_error(error, ctx);
            inner.attributes.append(&mut new_attributes);
        }

        let attrs = std::mem::take(&mut inner.attributes);
        inner.send_event(attrs);
    }
}

impl<A, T, Request, Response, EventResponse>
    CustomEventInner<Request, Response, EventResponse, A, T>
where
    A: Selectors<Request, Response, EventResponse> + Default,
    T: Selector<Request = Request, Response = Response> + Debug + Debug,
{
    #[inline]
    fn send_event(&self, attributes: Vec<KeyValue>) {
        log_event(self.level, &self.name, attributes, &self.message);
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
        EventLevel::Off => {}
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use apollo_compiler::name;
    use apollo_federation::sources::connect::ConnectId;
    use apollo_federation::sources::connect::ConnectSpec;
    use apollo_federation::sources::connect::Connector;
    use apollo_federation::sources::connect::HTTPMethod;
    use apollo_federation::sources::connect::HttpJsonTransport;
    use apollo_federation::sources::connect::JSONSelection;
    use apollo_federation::sources::connect::URLTemplate;
    use http::HeaderValue;
    use http::header::CONTENT_LENGTH;
    use router::body;
    use tracing::instrument::WithSubscriber;

    use super::*;
    use crate::assert_snapshot_subscriber;
    use crate::context::CONTAINS_GRAPHQL_ERROR;
    use crate::context::OPERATION_NAME;
    use crate::graphql;
    use crate::plugins::connectors::handle_responses::MappedResponse;
    use crate::plugins::connectors::make_requests::ResponseKey;
    use crate::plugins::telemetry::Telemetry;
    use crate::plugins::test::PluginTestHarness;
    use crate::services::connector::request_service::Request;
    use crate::services::connector::request_service::Response;
    use crate::services::connector::request_service::TransportRequest;
    use crate::services::connector::request_service::TransportResponse;
    use crate::services::connector::request_service::transport;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_router_events() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../testdata/custom_events.router.yaml"))
            .build()
            .await;

        async {
            test_harness
                .router_service(
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
                .call(
                    router::Request::fake_builder()
                        .header(CONTENT_LENGTH, "0")
                        .header("custom-header", "val1")
                        .header("x-log-request", HeaderValue::from_static("log"))
                        .build()
                        .unwrap()
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
                .router_service(

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
                .call(router::Request::fake_builder()
                    .header("custom-header", "val1")
                    .build()
                    .unwrap())
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
                .router_service(
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
                .call(router::Request::fake_builder()
                    .header("custom-header", "val1")
                    .build()
                    .unwrap())
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
                .supergraph_service(|_r| async {
                    supergraph::Response::fake_builder()
                        .header("custom-header", "val1")
                        .header("x-log-request", HeaderValue::from_static("log"))
                        .data(serde_json::json!({"data": "res"}).to_string())
                        .build()
                })
                .call(
                    supergraph::Request::fake_builder()
                        .query("query { foo }")
                        .header("x-log-request", HeaderValue::from_static("log"))
                        .build()
                        .unwrap(),
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
                .supergraph_service(|_r| async {
                    supergraph::Response::fake_builder()
                        .data(serde_json::json!({"data": "res"}).to_string())
                        .build()
                })
                .call(
                    supergraph::Request::fake_builder()
                        .query("query Test { foo }")
                        .context(ctx)
                        .build()
                        .unwrap(),
                )
                .await
                .expect("expecting successful response");
            test_harness
                .supergraph_service(|_r| async {
                    Ok(supergraph::Response::fake_builder()
                        .data(serde_json::json!({"data": "res"}).to_string())
                        .build()
                        .expect("expecting valid response"))
                })
                .call(
                    supergraph::Request::fake_builder()
                        .query("query { foo }")
                        .build()
                        .unwrap(),
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
                .supergraph_service(|_r| async {
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
                })
                .call(
                    supergraph::Request::fake_builder()
                        .query("query { foo }")
                        .build()
                        .unwrap(),
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
                .supergraph_service(|_r| async {
                    supergraph::Response::fake_builder()
                        .header("custom-header", "val1")
                        .header("x-log-response", HeaderValue::from_static("log"))
                        .data(serde_json_bytes::json!({"errors": [{"message": "res"}]}))
                        .build()
                })
                .call(
                    supergraph::Request::fake_builder()
                        .query("query { foo }")
                        .build()
                        .unwrap(),
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
                .subgraph_service("subgraph", |_r| async {
                    subgraph::Response::fake2_builder()
                        .header("custom-header", "val1")
                        .header("x-log-request", HeaderValue::from_static("log"))
                        .data(serde_json::json!({"data": "res"}).to_string())
                        .build()
                })
                .call(
                    subgraph::Request::fake_builder()
                        .subgraph_name("subgraph")
                        .subgraph_request(subgraph_req)
                        .build(),
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
                .subgraph_service("subgraph", |_r| async {
                    subgraph::Response::fake2_builder()
                        .header("custom-header", "val1")
                        .header("x-log-response", HeaderValue::from_static("log"))
                        .subgraph_name("subgraph")
                        .data(serde_json::json!({"data": "res"}).to_string())
                        .build()
                })
                .call(
                    subgraph::Request::fake_builder()
                        .subgraph_name("subgraph")
                        .subgraph_request(subgraph_req)
                        .build(),
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_connector_events_request() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../testdata/custom_events.router.yaml"))
            .build()
            .await;

        async {
            let context = crate::Context::default();
            let mut http_request = http::Request::builder().body("".into()).unwrap();
            http_request
                .headers_mut()
                .insert("x-log-request", HeaderValue::from_static("log"));
            let transport_request = TransportRequest::Http(transport::http::HttpRequest {
                inner: http_request,
                debug: None,
            });
            let connector = Connector {
                id: ConnectId::new(
                    "subgraph".into(),
                    Some("source".into()),
                    name!(Query),
                    name!(users),
                    0,
                    "label",
                ),
                transport: HttpJsonTransport {
                    source_url: None,
                    connect_template: URLTemplate::from_str("/test").unwrap(),
                    method: HTTPMethod::Get,
                    headers: Default::default(),
                    body: None,
                },
                selection: JSONSelection::empty(),
                config: None,
                max_requests: None,
                entity_resolver: None,
                spec: ConnectSpec::V0_1,
                request_variables: Default::default(),
                response_variables: Default::default(),
            };
            let response_key = ResponseKey::RootField {
                name: "hello".to_string(),
                inputs: Default::default(),
                selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
            };
            let connector_request = Request {
                context: context.clone(),
                connector: Arc::new(connector.clone()),
                service_name: Default::default(),
                transport_request,
                key: response_key.clone(),
                mapping_problems: vec![],
            };
            test_harness
                .call_connector_request_service(connector_request, |request| Response {
                    context: request.context.clone(),
                    connector: request.connector.clone(),
                    transport_result: Ok(TransportResponse::Http(transport::http::HttpResponse {
                        inner: http::Response::builder()
                            .status(200)
                            .header("x-log-request", HeaderValue::from_static("log"))
                            .body(body::empty())
                            .expect("expecting valid response")
                            .into_parts()
                            .0,
                    })),
                    mapped_response: MappedResponse::Data {
                        data: serde_json::json!({})
                            .try_into()
                            .expect("expecting valid JSON"),
                        key: request.key.clone(),
                        problems: vec![],
                    },
                })
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_connector_events_response() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../testdata/custom_events.router.yaml"))
            .build()
            .await;

        async {
            let context = crate::Context::default();
            let mut http_request = http::Request::builder().body("".into()).unwrap();
            http_request
                .headers_mut()
                .insert("x-log-response", HeaderValue::from_static("log"));
            let transport_request = TransportRequest::Http(transport::http::HttpRequest {
                inner: http_request,
                debug: None,
            });
            let connector = Connector {
                id: ConnectId::new(
                    "subgraph".into(),
                    Some("source".into()),
                    name!(Query),
                    name!(users),
                    0,
                    "label",
                ),
                transport: HttpJsonTransport {
                    source_url: None,
                    connect_template: URLTemplate::from_str("/test").unwrap(),
                    method: HTTPMethod::Get,
                    headers: Default::default(),
                    body: None,
                },
                selection: JSONSelection::empty(),
                config: None,
                max_requests: None,
                entity_resolver: None,
                spec: ConnectSpec::V0_1,
                request_variables: Default::default(),
                response_variables: Default::default(),
            };
            let response_key = ResponseKey::RootField {
                name: "hello".to_string(),
                inputs: Default::default(),
                selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
            };
            let connector_request = Request {
                context: context.clone(),
                connector: Arc::new(connector.clone()),
                service_name: Default::default(),
                transport_request,
                key: response_key.clone(),
                mapping_problems: vec![],
            };
            test_harness
                .call_connector_request_service(connector_request, |request| Response {
                    context: request.context.clone(),
                    connector: request.connector.clone(),
                    transport_result: Ok(TransportResponse::Http(transport::http::HttpResponse {
                        inner: http::Response::builder()
                            .status(200)
                            .header("x-log-response", HeaderValue::from_static("log"))
                            .body(body::empty())
                            .expect("expecting valid response")
                            .into_parts()
                            .0,
                    })),
                    mapped_response: MappedResponse::Data {
                        data: serde_json::json!({})
                            .try_into()
                            .expect("expecting valid JSON"),
                        key: request.key.clone(),
                        problems: vec![],
                    },
                })
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }
}
