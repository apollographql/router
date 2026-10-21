use std::collections::HashMap;
use std::sync::Arc;

use ::tracing::Span;
use opentelemetry::KeyValue;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::Context;
use crate::layers::ServiceBuilderExt;
use crate::plugins::telemetry::Telemetry;
use crate::plugins::telemetry::config;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::apollo::instruments::ApolloConnectorFetchDuration;
use crate::plugins::telemetry::config_new::apollo::instruments::ApolloConnectorInstruments;
use crate::plugins::telemetry::config_new::connector::events::ConnectorEvents;
use crate::plugins::telemetry::config_new::connector::instruments::ConnectorInstruments;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::instruments::StaticInstrument;
use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;
use crate::plugins::telemetry::error_counter::count_connector_errors;
use crate::plugins::telemetry::span_factory;
use crate::services::connector;

/// Layer type for [Telemetry::instrument_connector_layer].
#[derive(Clone)]
pub(crate) struct InstrumentConnectorLayer {
    config: Arc<config::Conf>,
    static_connector_instruments: Arc<HashMap<String, StaticInstrument>>,
    apollo_fetch_duration: Option<Arc<ApolloConnectorFetchDuration>>,
}

impl InstrumentConnectorLayer {
    fn new(
        config: Arc<config::Conf>,
        static_connector_instruments: Arc<HashMap<String, StaticInstrument>>,
        static_apollo_connector_instruments: &HashMap<String, StaticInstrument>,
    ) -> Self {
        // The fetch duration is only exported to Apollo, so don't record it without Apollo.
        let apollo_fetch_duration = config
            .apollo
            .exports_to_apollo()
            .then(|| {
                ApolloConnectorInstruments::fetch_duration(
                    static_apollo_connector_instruments,
                    &config.apollo,
                )
            })
            .flatten()
            .map(Arc::new);
        Self {
            config,
            static_connector_instruments,
            apollo_fetch_duration,
        }
    }
}

impl<S> tower::Layer<S> for InstrumentConnectorLayer
where
    S: tower::Service<
            connector::request_service::Request,
            Response = connector::request_service::Response,
            Error = BoxError,
        > + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = connector::request_service::BoxCloneService;

    fn layer(&self, service: S) -> Self::Service {
        let req_fn_config = self.config.clone();
        let res_fn_config = self.config.clone();
        let static_connector_instruments = self.static_connector_instruments.clone();
        let apollo_fetch_duration = self.apollo_fetch_duration.clone();
        ServiceBuilder::new()
            .instrument(move |req: &connector::request_service::Request| {
                span_factory::create_connector(req.connector.source_config_key().as_str())
            })
            .map_future_with_request_data(
                move |request: &connector::request_service::Request| {
                    let custom_instruments = req_fn_config
                        .instrumentation
                        .instruments
                        .new_connector_instruments(static_connector_instruments.clone());
                    custom_instruments.on_request(request);
                    let apollo_instruments = req_fn_config
                        .instrumentation
                        .instruments
                        .new_apollo_connector_instruments(apollo_fetch_duration.as_deref());
                    apollo_instruments.on_request(request);
                    let mut custom_events =
                        req_fn_config.instrumentation.events.new_connector_events();
                    custom_events.on_request(request);

                    let custom_span_attributes = req_fn_config
                        .instrumentation
                        .spans
                        .connector
                        .attributes
                        .on_request(request);

                    (
                        request.context.clone(),
                        custom_instruments,
                        apollo_instruments,
                        custom_events,
                        custom_span_attributes,
                    )
                },
                move |(
                    context,
                    custom_instruments,
                    apollo_connector_instruments,
                    mut custom_events,
                    custom_span_attributes,
                ): (
                    Context,
                    ConnectorInstruments,
                    ApolloConnectorInstruments,
                    ConnectorEvents,
                    Vec<KeyValue>,
                ),
                      f| {
                    let conf = res_fn_config.clone();
                    async move {
                        let span = Span::current();
                        span.set_span_dyn_attributes(custom_span_attributes);

                        let result = f.await;
                        match &result {
                            Ok(response) => {
                                span.set_span_dyn_attributes(
                                    conf.instrumentation
                                        .spans
                                        .connector
                                        .attributes
                                        .on_response(response),
                                );
                                custom_instruments.on_response(response);
                                apollo_connector_instruments.on_response(response);
                                custom_events.on_response(response);
                                // Errors the mapping declared with `->withError`.
                                // Counted here, before `include_subgraph_errors`
                                // decides what the client sees, so that
                                // client-facing redaction cannot suppress a
                                // metric — the same reason subgraph errors are
                                // counted at the subgraph layer. The connector
                                // span is current, so any error event lands on it.
                                count_connector_errors(response, &conf.apollo.errors);
                            }
                            Err(err) => {
                                span.set_span_dyn_attributes(
                                    conf.instrumentation
                                        .spans
                                        .connector
                                        .attributes
                                        .on_error(err, &context),
                                );
                                custom_instruments.on_error(err, &context);
                                apollo_connector_instruments.on_error(err, &context);
                                custom_events.on_error(err, &context);
                            }
                        }
                        result
                    }
                },
            )
            .service(service)
            .boxed_clone()
    }
}

impl Telemetry {
    /// Returns a layer that instruments a connector request service with both Apollo and custom
    /// instrumentation.
    pub(crate) fn instrument_connector_layer(&self) -> InstrumentConnectorLayer {
        let static_connector_instruments = self
            .builtin_instruments
            .read()
            .connector_custom_instruments
            .clone();
        let static_apollo_connector_instruments = self
            .builtin_instruments
            .read()
            .apollo_connector_instruments
            .clone();
        InstrumentConnectorLayer::new(
            self.config.clone(),
            static_connector_instruments,
            &static_apollo_connector_instruments,
        )
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
    use apollo_federation::connectors::runtime::key::ResponseKey;
    use apollo_federation::connectors::runtime::responses::MappedResponse;
    use tower::Service as _;

    use super::*;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::test::PluginTestHarness;
    use crate::services::connector::request_service::Request;
    use crate::services::connector::request_service::Response;
    use crate::services::connector::request_service::TransportOutcome;
    use crate::services::router::body;

    /// Sends one request for a connector of the `products` subgraph through
    /// `instrument_connector_layer`.
    async fn instrumented_connector_request(telemetry: &Telemetry) {
        let connector = Connector {
            id: ConnectId::new(
                "products".into(),
                Some(SourceName::cast("source")),
                name!(Query),
                name!(users),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: None,
                connect_template: StringTemplate::from_str("/test").unwrap(),
                ..Default::default()
            }),
            selection: JSONSelection::empty(),
            config: None,
            max_requests: None,
            entity_resolver: None,
            spec: ConnectSpec::V0_1,
            schema_subtypes_map: Default::default(),
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            output_type: None,
            label: "label".into(),
        };
        let request = Request {
            context: crate::Context::default(),
            connector: Arc::new(connector),
            transport_request: TransportRequest::Http(Box::new(HttpRequest {
                inner: http::Request::builder().body("".into()).unwrap(),
                debug: Default::default(),
            })),
            key: ResponseKey::RootField {
                name: "hello".to_string(),
                inputs: Default::default(),
                selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
            },
            mapping_problems: vec![],
            supergraph_request: Default::default(),
            operation: Default::default(),
        };

        let (mock_service, mut handle) = tower_test::mock::pair::<Request, Response>();
        let driver = tokio::spawn(async move {
            let (request, responder) = handle.next_request().await.unwrap();
            responder.send_response(Response {
                context: request.context.clone(),
                subgraph_name: request.connector.id.subgraph_name.to_string(),
                transport_outcome: TransportOutcome::Response(HttpResponse {
                    inner: http::Response::builder()
                        .status(200)
                        .body(body::empty())
                        .unwrap()
                        .into_parts()
                        .0,
                }),
                mapped_response: MappedResponse::Data {
                    data: serde_json::json!({}).try_into().unwrap(),
                    key: request.key.clone(),
                    problems: vec![],
                    declared_errors: vec![],
                },
                answered_by_router: false,
            });
        });
        let mut service = ServiceBuilder::new()
            .layer(telemetry.instrument_connector_layer())
            .service(mock_service);
        service.ready().await.unwrap().call(request).await.unwrap();
        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_fetch_duration_is_not_recorded_without_apollo() {
        async {
            let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
                .build()
                .await
                .expect("test harness");

            instrumented_connector_request(&test_harness).await;

            assert!(
                crate::metrics::collect_metrics()
                    .find("apollo.router.operations.fetch.duration")
                    .is_none()
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apollo_fetch_duration_is_recorded_when_exporting_to_apollo() {
        async {
            let telemetry = crate::plugins::telemetry::telemetry_exporting_to_apollo("").await;

            instrumented_connector_request(&telemetry).await;

            assert_histogram_count!(
                "apollo.router.operations.fetch.duration",
                1,
                "connector.source" = "source",
                "has_errors" = false,
                "subgraph.name" = "products"
            );
        }
        .with_metrics()
        .await;
    }
}
