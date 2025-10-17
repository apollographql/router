use std::sync::Arc;

use opentelemetry::Key;
use opentelemetry::KeyValue;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::Context;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::connector::ConnectorRequest;
use crate::plugins::telemetry::config_new::connector::ConnectorResponse;
use crate::plugins::telemetry::config_new::connector::attributes::ConnectorAttributes;
use crate::plugins::telemetry::config_new::connector::selectors::ConnectorSelector;
use crate::plugins::telemetry::config_new::events::CustomEvent;
use crate::plugins::telemetry::config_new::events::CustomEvents;
use crate::plugins::telemetry::config_new::events::Event;
use crate::plugins::telemetry::config_new::events::EventLevel;
use crate::plugins::telemetry::config_new::events::StandardEvent;
use crate::plugins::telemetry::config_new::events::StandardEventConfig;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::extendable::Extendable;

#[derive(Clone)]
pub(crate) struct ConnectorEventRequest {
    // XXX(@IvanGoncharov): As part of removing Mutex from StandardEvent I moved it here
    // I think it's not nessary here but can't verify it right now, so in future can just wrap StandardEvent
    pub(crate) level: EventLevel,
    pub(crate) condition: Arc<Mutex<Condition<ConnectorSelector>>>,
}

#[derive(Clone)]
pub(crate) struct ConnectorEventResponse {
    // XXX(@IvanGoncharov): As part of removing Arc from StandardEvent I moved it here
    // I think it's not nessary here but can't verify it right now, so in future can just wrap StandardEvent
    pub(crate) level: EventLevel,
    pub(crate) condition: Arc<Condition<ConnectorSelector>>,
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ConnectorEventsConfig {
    /// Log the connector HTTP request
    pub(crate) request: StandardEventConfig<ConnectorSelector>,
    /// Log the connector HTTP response
    pub(crate) response: StandardEventConfig<ConnectorSelector>,
    /// Log the connector HTTP error
    pub(crate) error: StandardEventConfig<ConnectorSelector>,
}

pub(crate) type ConnectorEvents =
    CustomEvents<ConnectorRequest, ConnectorResponse, (), ConnectorAttributes, ConnectorSelector>;

pub(crate) fn new_connector_events(
    config: &Extendable<ConnectorEventsConfig, Event<ConnectorAttributes, ConnectorSelector>>,
) -> ConnectorEvents {
    let custom_events = config
        .custom
        .iter()
        .filter_map(|(name, config)| CustomEvent::from_config(name, config))
        .collect();

    ConnectorEvents {
        request: StandardEvent::from_config(&config.attributes.request),
        response: StandardEvent::from_config(&config.attributes.response),
        error: StandardEvent::from_config(&config.attributes.error),
        custom: custom_events,
    }
}

impl CustomEvents<ConnectorRequest, ConnectorResponse, (), ConnectorAttributes, ConnectorSelector> {
    pub(crate) fn on_request(&mut self, request: &ConnectorRequest) {
        // Any condition on the request is NOT evaluated here. It must be evaluated later when
        // getting the ConnectorEventRequest from the context. The request context is shared
        // between all connector requests, so any request could find this ConnectorEventRequest in
        // the context. Its presence on the context cannot be conditional on an individual request.
        if let Some(request_event) = self.request.take() {
            request.context.extensions().with_lock(|lock| {
                lock.insert(ConnectorEventRequest {
                    level: request_event.level,
                    condition: Arc::new(Mutex::new(request_event.condition)),
                })
            });
        }

        if let Some(response_event) = self.response.take() {
            request.context.extensions().with_lock(|lock| {
                lock.insert(ConnectorEventResponse {
                    level: response_event.level,
                    condition: Arc::new(response_event.condition),
                })
            });
        }

        for custom_event in &mut self.custom {
            custom_event.on_request(request);
        }
    }

    pub(crate) fn on_response(&mut self, response: &ConnectorResponse) {
        for custom_event in &mut self.custom {
            custom_event.on_response(response);
        }
    }

    pub(crate) fn on_error(&mut self, error: &BoxError, ctx: &Context) {
        if let Some(error_event) = &mut self.error
            && error_event.condition.evaluate_error(error, ctx)
        {
            log_event(
                error_event.level,
                "connector.http.error",
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

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use apollo_compiler::name;
    use apollo_federation::connectors::ConnectId;
    use apollo_federation::connectors::ConnectSpec;
    use apollo_federation::connectors::Connector;
    use apollo_federation::connectors::HttpJsonTransport;
    use apollo_federation::connectors::JSONSelection;
    use apollo_federation::connectors::SourceName;
    use apollo_federation::connectors::StringTemplate;
    use apollo_federation::connectors::runtime::http_json_transport::HttpRequest;
    use apollo_federation::connectors::runtime::http_json_transport::HttpResponse;
    use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
    use apollo_federation::connectors::runtime::http_json_transport::TransportResponse;
    use apollo_federation::connectors::runtime::key::ResponseKey;
    use apollo_federation::connectors::runtime::responses::MappedResponse;
    use http::HeaderValue;
    use tracing::instrument::WithSubscriber;

    use super::*;
    use crate::assert_snapshot_subscriber;
    use crate::plugins::telemetry::Telemetry;
    use crate::plugins::test::PluginTestHarness;
    use crate::services::connector::request_service::Request;
    use crate::services::connector::request_service::Response;
    use crate::services::router::body;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_connector_events_request() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!("../../testdata/custom_events.router.yaml"))
            .build()
            .await
            .expect("test harness");

        async {
            let context = crate::Context::default();
            let mut http_request = http::Request::builder().body("".into()).unwrap();
            http_request
                .headers_mut()
                .insert("x-log-request", HeaderValue::from_static("log"));
            let transport_request = TransportRequest::Http(HttpRequest {
                inner: http_request,
                debug: Default::default(),
            });
            let connector = Connector {
                id: ConnectId::new(
                    "subgraph".into(),
                    Some(SourceName::cast("source")),
                    name!(Query),
                    name!(users),
                    None,
                    0,
                ),
                transport: HttpJsonTransport {
                    source_template: None,
                    connect_template: StringTemplate::from_str("/test").unwrap(),
                    ..Default::default()
                },
                selection: JSONSelection::empty(),
                config: None,
                max_requests: None,
                entity_resolver: None,
                spec: ConnectSpec::V0_1,
                batch_settings: None,
                request_headers: Default::default(),
                response_headers: Default::default(),
                request_variable_keys: Default::default(),
                response_variable_keys: Default::default(),
                error_settings: Default::default(),
                label: "label".into(),
            };
            let response_key = ResponseKey::RootField {
                name: "hello".to_string(),
                inputs: Default::default(),
                selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
            };
            let connector_request = Request {
                context: context.clone(),
                connector: Arc::new(connector.clone()),
                transport_request,
                key: response_key.clone(),
                mapping_problems: vec![],
                supergraph_request: Default::default(),
            };
            test_harness
                .call_connector_request_service(connector_request, |request| Response {
                    transport_result: Ok(TransportResponse::Http(HttpResponse {
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
            .config(include_str!("../../testdata/custom_events.router.yaml"))
            .build()
            .await
            .expect("test harness");

        async {
            let context = crate::Context::default();
            let mut http_request = http::Request::builder().body("".into()).unwrap();
            http_request
                .headers_mut()
                .insert("x-log-response", HeaderValue::from_static("log"));
            let transport_request = TransportRequest::Http(HttpRequest {
                inner: http_request,
                debug: Default::default(),
            });
            let connector = Connector {
                id: ConnectId::new(
                    "subgraph".into(),
                    Some(SourceName::cast("source")),
                    name!(Query),
                    name!(users),
                    None,
                    0,
                ),
                transport: HttpJsonTransport {
                    source_template: None,
                    connect_template: StringTemplate::from_str("/test").unwrap(),
                    ..Default::default()
                },
                selection: JSONSelection::empty(),
                config: None,
                max_requests: None,
                entity_resolver: None,
                spec: ConnectSpec::V0_1,
                batch_settings: None,
                request_headers: Default::default(),
                response_headers: Default::default(),
                request_variable_keys: Default::default(),
                response_variable_keys: Default::default(),
                error_settings: Default::default(),
                label: "label".into(),
            };
            let response_key = ResponseKey::RootField {
                name: "hello".to_string(),
                inputs: Default::default(),
                selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
            };
            let connector_request = Request {
                context: context.clone(),
                connector: Arc::new(connector.clone()),
                transport_request,
                key: response_key.clone(),
                mapping_problems: vec![],
                supergraph_request: Default::default(),
            };
            test_harness
                .call_connector_request_service(connector_request, |request| Response {
                    transport_result: Ok(TransportResponse::Http(HttpResponse {
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
