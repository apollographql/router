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
    static_apollo_connector_instruments: Arc<HashMap<String, StaticInstrument>>,
}

impl InstrumentConnectorLayer {
    fn new(
        config: Arc<config::Conf>,
        static_connector_instruments: Arc<HashMap<String, StaticInstrument>>,
        static_apollo_connector_instruments: Arc<HashMap<String, StaticInstrument>>,
    ) -> Self {
        Self {
            config,
            static_connector_instruments,
            static_apollo_connector_instruments,
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
        // the instrument sets, built once for the source's stack and started per request
        let instruments = &self.config.instrumentation.instruments;
        let sets = Arc::new((
            instruments.connector_instrument_set(&self.static_connector_instruments),
            instruments.apollo_connector_instrument_set(
                &self.static_apollo_connector_instruments,
                self.config.apollo.subgraph_metrics,
            ),
        ));
        ServiceBuilder::new()
            .instrument(move |req: &connector::request_service::Request| {
                span_factory::create_connector(req.connector.source_config_key().as_str())
            })
            .map_future_with_request_data(
                move |request: &connector::request_service::Request| {
                    let (connector_instrument_set, apollo_instrument_set) = &*sets;
                    let custom_instruments = connector_instrument_set.start();
                    custom_instruments.on_request(request);
                    let apollo_instruments = apollo_instrument_set.start();
                    apollo_instruments.on_request(request);
                    let mut custom_events =
                        req_fn_config.instrumentation.events.new_connector_events();
                    custom_events.on_request(request);

                    // the connector span's attributes, made only where the span is built
                    let custom_span_attributes = if Span::current().is_disabled() {
                        Vec::new()
                    } else {
                        req_fn_config
                            .instrumentation
                            .spans
                            .connector
                            .attributes
                            .on_request(request)
                    };

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
                                span.set_span_dyn_attributes_from(|| {
                                    conf.instrumentation
                                        .spans
                                        .connector
                                        .attributes
                                        .on_response(response)
                                });
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
                                span.set_span_dyn_attributes_from(|| {
                                    conf.instrumentation
                                        .spans
                                        .connector
                                        .attributes
                                        .on_error(err, &context)
                                });
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
            static_apollo_connector_instruments,
        )
    }
}

// The allocation control reads the router's tracking allocator, which these features build.
#[cfg(all(test, feature = "global-allocator", not(feature = "dhat-heap"), unix))]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::name;
    use apollo_federation::connectors::ConnectId;
    use apollo_federation::connectors::ConnectSpec;
    use apollo_federation::connectors::Connector;
    use apollo_federation::connectors::HttpJsonTransport;
    use apollo_federation::connectors::JSONSelection;
    use apollo_federation::connectors::runtime::http_json_transport::HttpRequest;
    use apollo_federation::connectors::runtime::http_json_transport::HttpResponse;
    use apollo_federation::connectors::runtime::key::ResponseKey;
    use apollo_federation::connectors::runtime::responses::MappedResponse;

    use super::*;
    use crate::plugins::test::PluginTestHarness;
    use crate::services::connector::request_service::Request;
    use crate::services::connector::request_service::Response;
    use crate::services::connector::request_service::TransportOutcome;

    /// A connector request to `subgraph_name`'s `Query.hello`.
    fn request(connector: &Arc<Connector>) -> Request {
        Request {
            context: Context::new(),
            connector: connector.clone(),
            transport_request: HttpRequest {
                inner: http::Request::builder().body("{}".to_string()).unwrap(),
                debug: Default::default(),
            }
            .into(),
            key: ResponseKey::RootField {
                name: "hello".to_string(),
                inputs: Default::default(),
                selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
            },
            mapping_problems: Default::default(),
            supergraph_request: Arc::new(
                http::Request::builder()
                    .body(crate::graphql::Request::builder().build())
                    .expect("valid request"),
            ),
            operation: Default::default(),
        }
    }

    /// A connector request through the connector instrument layer builds no instrument set: the
    /// layer builds the connector's instruments and Apollo's fetch duration, their conditions and
    /// their selectors once for its source's stack, and a request starts the state its
    /// measurements record into. Read as the bytes the layer allocates per request under a custom
    /// histogram whose value is a request header named at a width of 8 and of 2,056 bytes: a
    /// request that builds the custom instruments copies the selector, and the header's name with
    /// it, and one that starts them shares it.
    #[test]
    fn a_connector_request_builds_no_instrument_set() {
        const REQUESTS: usize = 16;
        const NARROW: usize = 8;
        const WIDE: usize = 2056;

        // the plugin's construction blocks in place, which a multi-threaded runtime allows
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime");
        let _entered = runtime.enter();
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_1,
            schema_subtypes_map: Default::default(),
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(hello),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            }),
            selection: JSONSelection::parse("$.data").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            output_type: None,
            label: "test label".into(),
        });
        let bytes = |width: usize| {
            let config = serde_json::json!({"telemetry": {"instrumentation": {"instruments": {
                "connector": {
                    "acme.request.header": {
                        "description": "a connector request header's value",
                        "type": "histogram",
                        "unit": "unit",
                        "value": {"connector_http_request_header": "h".repeat(width)}
                    }
                }
            }}}});
            let test_harness: PluginTestHarness<Telemetry> = runtime
                .block_on(
                    PluginTestHarness::builder()
                        .config(&config.to_string())
                        .build(),
                )
                .expect("test harness");
            let layered = ServiceBuilder::new()
                .layer(test_harness.instrument_connector_layer())
                .service(tower::service_fn(|request: Request| {
                    let (parts, _) = http::Response::builder().body(()).unwrap().into_parts();
                    std::future::ready(Ok::<_, BoxError>(Response {
                        context: request.context,
                        subgraph_name: request.connector.id.subgraph_name.to_string(),
                        transport_outcome: TransportOutcome::Response(HttpResponse {
                            inner: parts,
                        }),
                        mapped_response: MappedResponse::Data {
                            data: serde_json_bytes::Value::Null,
                            key: request.key,
                            problems: vec![],
                            declared_errors: vec![],
                        },
                        answered_by_router: false,
                    }))
                }));
            crate::allocator::bytes_per_call(layered, || request(&connector), drop, REQUESTS)
        };

        let narrow = bytes(NARROW);
        let wide = bytes(WIDE);
        eprintln!(
            "a connector request allocates {narrow} bytes at a header name of {NARROW} bytes and {wide} at {WIDE}"
        );
        assert!(
            wide.saturating_sub(narrow) < WIDE - NARROW,
            "a connector request allocates {wide} bytes at a header name of {WIDE} bytes and \
             {narrow} at {NARROW}: a request builds the custom instruments"
        );
    }
}
