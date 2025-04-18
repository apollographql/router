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
use crate::Context;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::connector::attributes::ConnectorAttributes;
use crate::plugins::telemetry::config_new::connector::events::ConnectorEvents;
use crate::plugins::telemetry::config_new::connector::events::ConnectorEventsConfig;
use crate::plugins::telemetry::config_new::connector::selectors::ConnectorSelector;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::router::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::router::events::RouterEvents;
use crate::plugins::telemetry::config_new::router::events::RouterEventsConfig;
use crate::plugins::telemetry::config_new::selectors::RouterSelector;
use crate::plugins::telemetry::config_new::selectors::SubgraphSelector;
use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
use crate::plugins::telemetry::config_new::subgraph::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::subgraph::events::SubgraphEvents;
use crate::plugins::telemetry::config_new::subgraph::events::SubgraphEventsConfig;
use crate::plugins::telemetry::config_new::supergraph::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::supergraph::events::SupergraphEvents;
use crate::plugins::telemetry::config_new::supergraph::events::SupergraphEventsConfig;
use crate::plugins::telemetry::dynamic_attribute::EventDynAttribute;

#[derive(Clone)]
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
    use crate::services::router;
    use crate::services::subgraph;
    use crate::services::supergraph;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_router_events() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../testdata/custom_events.router.yaml"))
            .build()
            .await
            .expect("test harness");

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
            .await
            .expect("test harness");

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
            .await
            .expect("test harness");

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
            .await
            .expect("test harness");

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
            .await
            .expect("test harness");

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
            .await
            .expect("test harness");

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
            .await
            .expect("test harness");

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
            .await
            .expect("test harness");

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
            .await
            .expect("test harness");

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
            .await
            .expect("test harness");

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
                batch_settings: None,
                request_headers: Default::default(),
                response_headers: Default::default(),
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
                supergraph_request: Default::default(),
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
            .await
            .expect("test harness");

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
                batch_settings: None,
                request_headers: Default::default(),
                response_headers: Default::default(),
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
                supergraph_request: Default::default(),
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
