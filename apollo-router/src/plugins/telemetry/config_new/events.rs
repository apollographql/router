use std::fmt::Debug;
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

use super::instruments::Instrumented;
use super::Selector;
use super::Selectors;
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
use crate::Context;

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
}

pub(crate) type RouterEvents =
    CustomEvents<router::Request, router::Response, RouterAttributes, RouterSelector>;

pub(crate) type SupergraphEvents = CustomEvents<
    supergraph::Request,
    supergraph::Response,
    SupergraphAttributes,
    SupergraphSelector,
>;

pub(crate) type SubgraphEvents =
    CustomEvents<subgraph::Request, subgraph::Response, SubgraphAttributes, SubgraphSelector>;

pub(crate) struct CustomEvents<Request, Response, Attributes, Sel>
where
    Attributes: Selectors<Request = Request, Response = Response> + Default,
    Sel: Selector<Request = Request, Response = Response> + Debug,
{
    request: StandardEvent<Sel>,
    response: StandardEvent<Sel>,
    error: StandardEvent<Sel>,
    custom: Vec<CustomEvent<Request, Response, Attributes, Sel>>,
}

impl Instrumented
    for CustomEvents<router::Request, router::Response, RouterAttributes, RouterSelector>
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
            attrs.push(KeyValue::new(
                Key::from_static_str("http.response.body"),
                opentelemetry::Value::String(format!("{:?}", response.response.body()).into()),
            ));
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
            log_event(self.request.level(), "supergraph.request", attrs, "");
        }
        if self.response.level() != EventLevel::Off {
            request
                .context
                .extensions()
                .with_lock(|mut lock| lock.insert(SupergraphEventResponse(self.response.clone())));
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
    for CustomEvents<subgraph::Request, subgraph::Response, SubgraphAttributes, SubgraphSelector>
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
                .with_lock(|mut lock| lock.insert(SubgraphEventRequest(self.request.clone())));
        }
        if self.response.level() != EventLevel::Off {
            request
                .context
                .extensions()
                .with_lock(|mut lock| lock.insert(SubgraphEventResponse(self.response.clone())));
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
    level: EventLevel,

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

pub(crate) struct CustomEvent<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response> + Debug,
{
    inner: Mutex<CustomEventInner<Request, Response, A, T>>,
}

struct CustomEventInner<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response> + Debug,
{
    name: String,
    level: EventLevel,
    event_on: EventOn,
    message: Arc<String>,
    selectors: Option<Arc<Extendable<A, T>>>,
    condition: Condition<T>,
    attributes: Vec<opentelemetry::KeyValue>,
}

impl<A, T, Request, Response, EventResponse> Instrumented for CustomEvent<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response, EventResponse = EventResponse> + Default,
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

        inner.send_event(attributes);
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

impl<A, T, Request, Response> CustomEventInner<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
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
    use http::header::CONTENT_LENGTH;
    use http::HeaderValue;
    use tracing::instrument::WithSubscriber;

    use super::*;
    use crate::assert_snapshot_subscriber;
    use crate::context::CONTAINS_GRAPHQL_ERROR;
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
                    |_r| {
                        router::Response::fake_builder()
                            .header("custom-header", "val1")
                            .header(CONTENT_LENGTH, "25")
                            .header("x-log-request", HeaderValue::from_static("log"))
                            .data(serde_json_bytes::json!({"data": "res"}))
                            .build()
                            .expect("expecting valid response")
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
                    |_r| {
                        let context_with_error = Context::new();
                        let _ = context_with_error
                            .insert(CONTAINS_GRAPHQL_ERROR, true)
                            .unwrap();
                        router::Response::fake_builder()
                            .header("custom-header", "val1")
                            .context(context_with_error)
                            .data(serde_json_bytes::json!({"errors": [{"message": "res"}]}))
                            .build()
                            .expect("expecting valid response")
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
                    |_r| {
                        router::Response::fake_builder()
                            .header("custom-header", "val1")
                            .header(CONTENT_LENGTH, "25")
                            .header("x-log-response", HeaderValue::from_static("log"))
                            .data(serde_json_bytes::json!({"data": "res"}))
                            .build()
                            .expect("expecting valid response")
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
                    |_r| {
                        supergraph::Response::fake_builder()
                            .header("custom-header", "val1")
                            .header("x-log-request", HeaderValue::from_static("log"))
                            .data(serde_json::json!({"data": "res"}).to_string())
                            .build()
                            .expect("expecting valid response")
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
                    |_r| {
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
                            .expect("expecting valid response")
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
                    |_r| {
                        supergraph::Response::fake_builder()
                            .header("custom-header", "val1")
                            .header("x-log-response", HeaderValue::from_static("log"))
                            .data(serde_json_bytes::json!({"errors": [{"message": "res"}]}))
                            .build()
                            .expect("expecting valid response")
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
                    |_r| {
                        subgraph::Response::fake2_builder()
                            .header("custom-header", "val1")
                            .header("x-log-request", HeaderValue::from_static("log"))
                            .data(serde_json::json!({"data": "res"}).to_string())
                            .build()
                            .expect("expecting valid response")
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
                    |_r| {
                        subgraph::Response::fake2_builder()
                            .header("custom-header", "val1")
                            .header("x-log-response", HeaderValue::from_static("log"))
                            .data(serde_json::json!({"data": "res"}).to_string())
                            .build()
                            .expect("expecting valid response")
                    },
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }
}
