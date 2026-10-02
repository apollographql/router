use std::collections::HashMap;
use std::sync::Arc;

use ::tracing::Span;
use http::HeaderName;
use http::HeaderValue;
use http::StatusCode;
use opentelemetry::KeyValue;
use opentelemetry::trace::TraceContextExt;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use serde_json_bytes::json;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::Context;
use crate::layers::ServiceBuilderExt;
use crate::plugins::telemetry::EnableSubgraphFtv1;
use crate::plugins::telemetry::SUBGRAPH_FTV1;
use crate::plugins::telemetry::Telemetry;
use crate::plugins::telemetry::config;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::apollo::instruments::ApolloSubgraphInstruments;
use crate::plugins::telemetry::config_new::cache::CacheInstruments;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::instruments::StaticInstrument;
use crate::plugins::telemetry::config_new::subgraph::events::SubgraphEvents;
use crate::plugins::telemetry::config_new::subgraph::instruments::SubgraphInstruments;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_ERROR;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_OK;
use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;
use crate::plugins::telemetry::error_counter::count_subgraph_errors;
use crate::plugins::telemetry::otel::OpenTelemetrySpanExt;
use crate::plugins::telemetry::span_factory;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::subgraph;

static FTV1_HEADER_NAME: HeaderName = HeaderName::from_static("apollo-federation-include-trace");
static FTV1_HEADER_VALUE: HeaderValue = HeaderValue::from_static("ftv1");

fn request_ftv1(mut req: SubgraphRequest) -> SubgraphRequest {
    if req
        .context
        .extensions()
        .with_lock(|lock| lock.contains_key::<EnableSubgraphFtv1>())
        && Span::current().context().span().span_context().is_sampled()
    {
        req.subgraph_request
            .headers_mut()
            .insert(FTV1_HEADER_NAME.clone(), FTV1_HEADER_VALUE.clone());
    }
    req
}

fn store_ftv1(subgraph_name: &ByteString, resp: SubgraphResponse) -> SubgraphResponse {
    // Stash the FTV1 data
    if resp
        .context
        .extensions()
        .with_lock(|lock| lock.contains_key::<EnableSubgraphFtv1>())
        && let Some(serde_json_bytes::Value::String(ftv1)) =
            resp.response.body().extensions.get("ftv1")
    {
        // Record the ftv1 trace for processing later
        Span::current().record("apollo_private.ftv1", ftv1.as_str());
        resp.context
            .upsert_json_value(SUBGRAPH_FTV1, move |value: Value| {
                let mut vec = match value {
                    Value::Array(array) => array,
                    // upsert_json_value populate the entry with null if it was vacant
                    Value::Null => Vec::new(),
                    _ => panic!("unexpected JSON value kind"),
                };
                vec.push(json!([subgraph_name, ftv1]));
                Value::Array(vec)
            })
    }
    resp
}

/// Layer type for [Telemetry::subgraph_ftv1_layer].
#[derive(Clone, Copy)]
pub(crate) struct SubgraphFtv1Layer {
    _private: (),
}

impl SubgraphFtv1Layer {
    fn new() -> Self {
        Self { _private: () }
    }
}

impl<S> tower::Layer<S> for SubgraphFtv1Layer
where
    S: tower::Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = subgraph::BoxCloneService;

    fn layer(&self, inner: S) -> Self::Service {
        ServiceBuilder::new()
            .map_request(request_ftv1)
            .map_response(|resp: SubgraphResponse| {
                let subgraph_name = ByteString::from(resp.subgraph_name.as_str());
                store_ftv1(&subgraph_name, resp)
            })
            .service(inner)
            .boxed_clone()
    }
}

/// Layer type for [Telemetry::instrument_subgraph_layer].
#[derive(Clone)]
pub(crate) struct InstrumentSubgraphLayer {
    config: Arc<config::Conf>,
    static_subgraph_instruments: Arc<HashMap<String, StaticInstrument>>,
    static_apollo_subgraph_instruments: Arc<HashMap<String, StaticInstrument>>,
    static_cache_instruments: Arc<HashMap<String, StaticInstrument>>,
}

impl InstrumentSubgraphLayer {
    fn new(
        config: Arc<config::Conf>,
        static_subgraph_instruments: Arc<HashMap<String, StaticInstrument>>,
        static_apollo_subgraph_instruments: Arc<HashMap<String, StaticInstrument>>,
        static_cache_instruments: Arc<HashMap<String, StaticInstrument>>,
    ) -> Self {
        Self {
            config,
            static_subgraph_instruments,
            static_apollo_subgraph_instruments,
            static_cache_instruments,
        }
    }
}

impl<S> tower::Layer<S> for InstrumentSubgraphLayer
where
    S: tower::Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = subgraph::BoxCloneService;

    fn layer(&self, inner: S) -> Self::Service {
        let req_fn_config = self.config.clone();
        let res_fn_config = self.config.clone();
        let static_subgraph_instruments = self.static_subgraph_instruments.clone();
        let static_apollo_subgraph_instruments = self.static_apollo_subgraph_instruments.clone();
        let static_cache_instruments = self.static_cache_instruments.clone();

        ServiceBuilder::new()
            .instrument(move |req: &SubgraphRequest| {
                span_factory::create_subgraph(req.subgraph_name.as_str(), req)
            })
            .map_future_with_request_data(
                move |sub_request: &SubgraphRequest| {
                    let custom_attributes = req_fn_config
                        .instrumentation
                        .spans
                        .subgraph
                        .attributes
                        .on_request(sub_request);
                    let custom_instruments = req_fn_config
                        .instrumentation
                        .instruments
                        .new_subgraph_instruments(static_subgraph_instruments.clone());
                    custom_instruments.on_request(sub_request);
                    let mut custom_events =
                        req_fn_config.instrumentation.events.new_subgraph_events();
                    custom_events.on_request(sub_request);

                    let apollo_instruments: ApolloSubgraphInstruments = req_fn_config
                        .instrumentation
                        .instruments
                        .new_apollo_subgraph_instruments(
                            static_apollo_subgraph_instruments.clone(),
                            req_fn_config.apollo.clone(),
                        );
                    apollo_instruments.on_request(sub_request);

                    let custom_cache_instruments: CacheInstruments = req_fn_config
                        .instrumentation
                        .instruments
                        .new_cache_instruments(static_cache_instruments.clone());
                    custom_cache_instruments.on_request(sub_request);

                    (
                        sub_request.context.clone(),
                        custom_instruments,
                        custom_attributes,
                        custom_events,
                        apollo_instruments,
                        custom_cache_instruments,
                    )
                },
                move |(
                    context,
                    custom_instruments,
                    custom_attributes,
                    mut custom_events,
                    apollo_instruments,
                    custom_cache_instruments,
                ): (
                    Context,
                    SubgraphInstruments,
                    Vec<KeyValue>,
                    SubgraphEvents,
                    ApolloSubgraphInstruments,
                    CacheInstruments,
                ),
                      f| {
                    let conf = res_fn_config.clone();
                    async move {
                        let span = Span::current();
                        span.set_span_dyn_attributes(custom_attributes);
                        let result: Result<SubgraphResponse, BoxError> = f.await;

                        match &result {
                            Ok(resp) => {
                                if resp.response.status() >= StatusCode::BAD_REQUEST {
                                    span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                                } else {
                                    span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_OK);
                                }
                                span.set_span_dyn_attributes(
                                    conf.instrumentation
                                        .spans
                                        .subgraph
                                        .attributes
                                        .on_response(resp),
                                );
                                apollo_instruments.on_response(resp);
                                custom_cache_instruments.on_response(resp);
                                custom_instruments.on_response(resp);
                                custom_events.on_response(resp);
                            }
                            Err(err) => {
                                span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                                span.set_span_dyn_attributes(
                                    conf.instrumentation
                                        .spans
                                        .subgraph
                                        .attributes
                                        .on_error(err, &context),
                                );
                                apollo_instruments.on_error(err, &context);
                                custom_cache_instruments.on_error(err, &context);
                                custom_instruments.on_error(err, &context);
                                custom_events.on_error(err, &context);
                            }
                        }

                        if let Ok(resp) = result {
                            Ok(count_subgraph_errors(resp, &conf.apollo.errors).await)
                        } else {
                            result
                        }
                    }
                },
            )
            .service(inner)
            .boxed_clone()
    }
}

impl Telemetry {
    /// Returns a layer that instruments a subgraph service with both Apollo and custom
    /// instrumentation.
    pub(crate) fn instrument_subgraph_layer(&self) -> InstrumentSubgraphLayer {
        let static_subgraph_instruments = self
            .builtin_instruments
            .read()
            .subgraph_custom_instruments
            .clone();
        let static_apollo_subgraph_instruments = self
            .builtin_instruments
            .read()
            .apollo_subgraph_instruments
            .clone();
        let static_cache_instruments = self
            .builtin_instruments
            .read()
            .cache_custom_instruments
            .clone();
        InstrumentSubgraphLayer::new(
            self.config.clone(),
            static_subgraph_instruments,
            static_apollo_subgraph_instruments,
            static_cache_instruments,
        )
    }

    /// Returns a layer that propagates FTV1 tracing headers to subgraph requests and stashes the
    /// traces from the response for processing.
    pub(crate) fn subgraph_ftv1_layer(&self) -> SubgraphFtv1Layer {
        SubgraphFtv1Layer::new()
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderMap;
    use http::header::CONTENT_TYPE;
    use tower::Service as _;

    use super::*;
    use crate::error::FetchError;
    use crate::graphql;
    use crate::graphql::Error;
    use crate::graphql::Request;
    use crate::http_ext;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::test::PluginTestHarness;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_custom_subgraph_instruments_level() {
        async {
            let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
                .config(include_str!(
                    "../testdata/custom_instruments_level.router.yaml"
                ))
                .build()
                .await
                .expect("test harness");

            let (mock_bad_request_service, mut handle) =
                tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
            let driver = tokio::spawn(async move {
                for _ in 0..2 {
                    let (req, responder) = handle.next_request().await.unwrap();
                    let mut headers = HeaderMap::new();
                    headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
                    let errors = vec![
                        graphql::Error::builder()
                            .message("nope".to_string())
                            .extension_code("NOPE")
                            .build(),
                        graphql::Error::builder()
                            .message("nok".to_string())
                            .extension_code("NOK")
                            .build(),
                    ];
                    responder.send_response(
                        SubgraphResponse::fake_builder()
                            .context(req.context)
                            .status_code(StatusCode::BAD_REQUEST)
                            .headers(headers)
                            .errors(errors)
                            .build(),
                    );
                }
            });
            let mut bad_request_subgraph_service = ServiceBuilder::new()
                .layer(test_harness.instrument_subgraph_layer())
                .service(mock_bad_request_service);
            let sub_req = http::Request::builder()
                .method("POST")
                .uri("http://test")
                .header("x-custom", "TEST")
                .header("conditional-custom", "X")
                .header("custom-length", "55")
                .header("content-length", "55")
                .header("content-type", "application/graphql")
                .body(graphql::Request::builder().query("{ me {name} }").build())
                .unwrap();
            let subgraph_req = SubgraphRequest::fake_builder()
                .subgraph_request(sub_req)
                .subgraph_name("test".to_string())
                .build();

            let _router_response = bad_request_subgraph_service
                .ready()
                .await
                .unwrap()
                .call(subgraph_req)
                .await
                .unwrap();

            assert_counter!(
                "acme.subgraph.error_reqs",
                1.0,
                graphql_error = opentelemetry::Value::Array(opentelemetry::Array::String(vec![
                    "nope".into(),
                    "nok".into()
                ])),
                subgraph.name = "test"
            );
            let sub_req = http::Request::builder()
                .method("POST")
                .uri("http://test")
                .header("x-custom", "TEST")
                .header("conditional-custom", "X")
                .header("custom-length", "55")
                .header("content-length", "55")
                .header("content-type", "application/graphql")
                .body(graphql::Request::builder().query("{ me {name} }").build())
                .unwrap();
            let subgraph_req = SubgraphRequest::fake_builder()
                .subgraph_request(sub_req)
                .subgraph_name("test".to_string())
                .build();

            let _router_response = bad_request_subgraph_service
                .ready()
                .await
                .unwrap()
                .call(subgraph_req)
                .await
                .unwrap();
            assert_counter!(
                "acme.subgraph.error_reqs",
                2.0,
                graphql_error = opentelemetry::Value::Array(opentelemetry::Array::String(vec![
                    "nope".into(),
                    "nok".into()
                ])),
                subgraph.name = "test"
            );
            assert_histogram_not_exists!("http.client.request.duration", f64);
            drop(bad_request_subgraph_service);
            crate::plugin::test::await_mock_driver(driver).await;
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_custom_subgraph_instruments() {
        async {
            let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
                .config(include_str!("../testdata/custom_instruments.router.yaml"))
                .build()
                .await
                .expect("test harness");

            let (mock_bad_request_service, mut handle) =
                tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
            let driver = tokio::spawn(async move {
                for _ in 0..2 {
                    let (req, responder) = handle.next_request().await.unwrap();
                    let mut headers = HeaderMap::new();
                    headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
                    let errors = vec![
                        graphql::Error::builder()
                            .message("nope".to_string())
                            .extension_code("NOPE")
                            .build(),
                        graphql::Error::builder()
                            .message("nok".to_string())
                            .extension_code("NOK")
                            .build(),
                    ];
                    responder.send_response(
                        SubgraphResponse::fake_builder()
                            .context(req.context)
                            .status_code(StatusCode::BAD_REQUEST)
                            .headers(headers)
                            .errors(errors)
                            .build(),
                    );
                }
            });
            let mut bad_request_subgraph_service = ServiceBuilder::new()
                .layer(test_harness.instrument_subgraph_layer())
                .service(mock_bad_request_service);
            let sub_req = http::Request::builder()
                .method("POST")
                .uri("http://test")
                .header("x-custom", "TEST")
                .header("conditional-custom", "X")
                .header("custom-length", "55")
                .header("content-length", "55")
                .header("content-type", "application/graphql")
                .body(graphql::Request::builder().query("{ me {name} }").build())
                .unwrap();
            let subgraph_req = SubgraphRequest::fake_builder()
                .subgraph_request(sub_req)
                .subgraph_name("test".to_string())
                .build();

            let _router_response = bad_request_subgraph_service
                .ready()
                .await
                .unwrap()
                .call(subgraph_req)
                .await
                .unwrap();

            assert_counter!(
                "acme.subgraph.error_reqs",
                1.0,
                graphql_error = opentelemetry::Value::Array(opentelemetry::Array::String(vec![
                    "nope".into(),
                    "nok".into()
                ])),
                subgraph.name = "test"
            );
            let sub_req = http::Request::builder()
                .method("POST")
                .uri("http://test")
                .header("x-custom", "TEST")
                .header("conditional-custom", "X")
                .header("custom-length", "55")
                .header("content-length", "55")
                .header("content-type", "application/graphql")
                .body(graphql::Request::builder().query("{ me {name} }").build())
                .unwrap();
            let subgraph_req = SubgraphRequest::fake_builder()
                .subgraph_request(sub_req)
                .subgraph_name("test".to_string())
                .build();

            let _router_response = bad_request_subgraph_service
                .ready()
                .await
                .unwrap()
                .call(subgraph_req)
                .await
                .unwrap();
            assert_counter!(
                "acme.subgraph.error_reqs",
                2.0,
                graphql_error = opentelemetry::Value::Array(opentelemetry::Array::String(vec![
                    "nope".into(),
                    "nok".into()
                ])),
                subgraph.name = "test"
            );
            drop(bad_request_subgraph_service);
            crate::plugin::test::await_mock_driver(driver).await;
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_metrics_ok() {
        async {
            let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
                .config(include_str!("../testdata/custom_attributes.router.yaml"))
                .build()
                .await
                .expect("test harness");

            let (mock_subgraph_service, mut handle) =
                tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
            let driver = tokio::spawn(async move {
                let (req, responder) = handle.next_request().await.unwrap();
                let mut extension = crate::json_ext::Object::new();
                extension.insert(
                    serde_json_bytes::ByteString::from("status"),
                    serde_json_bytes::Value::String(ByteString::from(
                        "custom_error_for_propagation",
                    )),
                );
                let _ = req
                    .context
                    .insert("my_key", "my_custom_attribute_from_context".to_string())
                    .unwrap();
                responder.send_response(
                    SubgraphResponse::fake_builder()
                        .context(req.context)
                        .error(
                            Error::builder()
                                .message(String::from("an error occured"))
                                .extensions(extension)
                                .extension_code("FETCH_ERROR")
                                .build(),
                        )
                        .build(),
                );
            });

            let mut subgraph_service = ServiceBuilder::new()
                .layer(test_harness.instrument_subgraph_layer())
                .service(mock_subgraph_service);
            let subgraph_req = SubgraphRequest::fake_builder()
                .subgraph_request(
                    http_ext::Request::fake_builder()
                        .header("test", "my_value_set")
                        .body(
                            Request::fake_builder()
                                .query(String::from("query { test }"))
                                .build(),
                        )
                        .build()
                        .unwrap(),
                )
                .subgraph_name("my_subgraph_name")
                .build();
            let _subgraph_response = subgraph_service
                .ready()
                .await
                .unwrap()
                .call(subgraph_req)
                .await
                .unwrap();

            assert_histogram_count!(
                "http.client.request.duration",
                1,
                "error" = "custom_error_for_propagation",
                "my_key" = "my_custom_attribute_from_context",
                "query_from_request" = "query { test }",
                "status" = 200,
                "subgraph" = "my_subgraph_name",
                "subgraph_error_extended_code" = "FETCH_ERROR"
            );
            crate::plugin::test::await_mock_driver(driver).await;
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_metrics_http_error() {
        async {
            let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
                .config(include_str!("../testdata/custom_attributes.router.yaml"))
                .build()
                .await
                .expect("test harness");

            let (mock_subgraph_service_in_error, mut handle) =
                tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
            let driver = tokio::spawn(async move {
                let (_req, responder) = handle.next_request().await.unwrap();
                responder.send_error(FetchError::SubrequestHttpError {
                    status_code: None,
                    service: String::from("my_subgraph_name_error"),
                    reason: String::from("cannot contact the subgraph"),
                });
            });

            let mut subgraph_service = ServiceBuilder::new()
                .layer(test_harness.instrument_subgraph_layer())
                .service(mock_subgraph_service_in_error);

            let subgraph_req = SubgraphRequest::fake_builder()
                .subgraph_request(
                    http_ext::Request::fake_builder()
                        .header("test", "my_value_set")
                        .body(
                            Request::fake_builder()
                                .query(String::from("query { test }"))
                                .build(),
                        )
                        .build()
                        .unwrap(),
                )
                .subgraph_name("my_subgraph_name_error")
                .build();
            let _subgraph_response = subgraph_service
                .ready()
                .await
                .unwrap()
                .call(subgraph_req)
                .await
                .expect_err("should be an error");

            assert_histogram_count!(
                "http.client.request.duration",
                1,
                "message" = "HTTP fetch failed: cannot contact the subgraph",
                "subgraph" = "my_subgraph_name_error",
                "query_from_request" = "query { test }"
            );
            crate::plugin::test::await_mock_driver(driver).await;
        }
        .with_metrics()
        .await;
    }

    mod subgraph_ftv1_layer {
        use opentelemetry::Context as OtelContext;
        use opentelemetry::trace::SpanContext;
        use opentelemetry::trace::SpanId;
        use opentelemetry::trace::TraceContextExt;
        use opentelemetry::trace::TraceFlags;
        use opentelemetry::trace::TraceId;
        use opentelemetry::trace::TraceState;
        use serde_json_bytes::json;
        use tracing::Instrument;
        use tracing_subscriber::layer::SubscriberExt;

        use super::*;
        use crate::plugins::telemetry::EnableSubgraphFtv1;
        use crate::plugins::telemetry::SUBGRAPH_FTV1;
        use crate::plugins::telemetry::otel;

        const FTV1_HEADER_NAME: &str = "apollo-federation-include-trace";

        const SUBGRAPH_NAME: &str = "test_subgraph";

        /// Sets up tracing + otel and returns an arbitrary span. The `sampled` argument controls if
        /// the span is sampled.
        fn setup_tracing_span_with_sampled(
            sampled: bool,
        ) -> (tracing::subscriber::DefaultGuard, tracing::Span) {
            let subscriber = tracing_subscriber::registry().with(otel::layer());
            let guard = tracing::subscriber::set_default(subscriber);

            // Create the span in a context with the right sampled flag
            let span_context = SpanContext::new(
                TraceId::from(42),
                SpanId::from(42),
                TraceFlags::default().with_sampled(sampled),
                false,
                TraceState::default(),
            );
            let _otel_guard = OtelContext::new()
                .with_remote_span_context(span_context)
                .attach();

            let span = tracing::span!(tracing::Level::INFO, "test");
            (guard, span)
        }

        fn context_with_ftv1() -> Context {
            let context = Context::new();
            context
                .extensions()
                .with_lock(|lock| lock.insert(EnableSubgraphFtv1));
            context
        }

        #[tokio::test]
        async fn adds_ftv1_header_when_enabled_and_sampled() {
            let (_subscriber_guard, span) = setup_tracing_span_with_sampled(true);
            async {
                let (mock_service, mut handle) =
                    tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
                let driver = tokio::spawn(async move {
                    let (req, responder) = handle.next_request().await.unwrap();
                    assert_eq!(
                        req.subgraph_request
                            .headers()
                            .get(FTV1_HEADER_NAME)
                            .map(|v| v.to_str().unwrap()),
                        Some("ftv1"),
                        "FTV1 header should be set on the subgraph request"
                    );
                    responder.send_response(
                        SubgraphResponse::fake_builder()
                            .context(req.context)
                            .subgraph_name(SUBGRAPH_NAME)
                            .build(),
                    );
                });

                let mut service = ServiceBuilder::new()
                    .layer(SubgraphFtv1Layer::new())
                    .service(mock_service);
                let request = SubgraphRequest::fake_builder()
                    .subgraph_name(SUBGRAPH_NAME)
                    .context(context_with_ftv1())
                    .build();

                service.ready().await.unwrap().call(request).await.unwrap();

                crate::plugin::test::await_mock_driver(driver).await;
            }
            .instrument(span)
            .await;
        }

        #[tokio::test]
        async fn skips_ftv1_header_when_disabled() {
            let (_subscriber_guard, span) = setup_tracing_span_with_sampled(true);
            async {
                let (mock_service, mut handle) =
                    tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
                let driver = tokio::spawn(async move {
                    let (req, responder) = handle.next_request().await.unwrap();
                    assert!(
                        req.subgraph_request
                            .headers()
                            .get(FTV1_HEADER_NAME)
                            .is_none(),
                        "FTV1 header should not be set when ftv1 is not enabled on the context"
                    );
                    responder.send_response(
                        SubgraphResponse::fake_builder()
                            .context(req.context)
                            .subgraph_name(SUBGRAPH_NAME)
                            .build(),
                    );
                });

                let mut service = ServiceBuilder::new()
                    .layer(SubgraphFtv1Layer::new())
                    .service(mock_service);
                let request = SubgraphRequest::fake_builder()
                    .subgraph_name(SUBGRAPH_NAME)
                    .context(Context::new())
                    .build();

                service.ready().await.unwrap().call(request).await.unwrap();

                crate::plugin::test::await_mock_driver(driver).await;
            }
            .instrument(span)
            .await;
        }

        #[tokio::test]
        async fn skips_ftv1_header_when_not_sampled() {
            let (_subscriber_guard, span) = setup_tracing_span_with_sampled(false);
            async {
                let (mock_service, mut handle) =
                    tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
                let driver = tokio::spawn(async move {
                    let (req, responder) = handle.next_request().await.unwrap();
                    assert!(
                        req.subgraph_request
                            .headers()
                            .get(FTV1_HEADER_NAME)
                            .is_none(),
                        "FTV1 header should not be set when the span is not sampled"
                    );
                    responder.send_response(
                        SubgraphResponse::fake_builder()
                            .context(req.context)
                            .subgraph_name(SUBGRAPH_NAME)
                            .build(),
                    );
                });

                let mut service = ServiceBuilder::new()
                    .layer(SubgraphFtv1Layer::new())
                    .service(mock_service);
                let request = SubgraphRequest::fake_builder()
                    .subgraph_name(SUBGRAPH_NAME)
                    .context(context_with_ftv1())
                    .build();

                service.ready().await.unwrap().call(request).await.unwrap();

                crate::plugin::test::await_mock_driver(driver).await;
            }
            .instrument(span)
            .await;
        }

        #[tokio::test]
        async fn stores_response_trace_when_enabled() {
            let (_subscriber_guard, span) = setup_tracing_span_with_sampled(true);
            async {
                let (mock_service, mut handle) =
                    tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
                let driver = tokio::spawn(async move {
                    let (req, responder) = handle.next_request().await.unwrap();
                    responder.send_response(
                        SubgraphResponse::fake_builder()
                            .context(req.context)
                            .subgraph_name(SUBGRAPH_NAME)
                            .extension("ftv1", "encoded-trace")
                            .build(),
                    );
                });

                let mut service = ServiceBuilder::new()
                    .layer(SubgraphFtv1Layer::new())
                    .service(mock_service);
                let request = SubgraphRequest::fake_builder()
                    .subgraph_name(SUBGRAPH_NAME)
                    .context(context_with_ftv1())
                    .build();

                let response = service.ready().await.unwrap().call(request).await.unwrap();

                let stored = response
                    .context
                    .get_json_value(SUBGRAPH_FTV1)
                    .expect("SUBGRAPH_FTV1 should be populated");
                assert_eq!(stored, json!([[SUBGRAPH_NAME, "encoded-trace"]]));

                crate::plugin::test::await_mock_driver(driver).await;
            }
            .instrument(span)
            .await;
        }

        #[tokio::test]
        async fn does_not_store_response_trace_when_not_enabled() {
            let (_subscriber_guard, span) = setup_tracing_span_with_sampled(true);
            async {
                let (mock_service, mut handle) =
                    tower_test::mock::pair::<SubgraphRequest, SubgraphResponse>();
                let driver = tokio::spawn(async move {
                    let (req, responder) = handle.next_request().await.unwrap();
                    responder.send_response(
                        SubgraphResponse::fake_builder()
                            .context(req.context)
                            .subgraph_name(SUBGRAPH_NAME)
                            .extension("ftv1", "encoded-trace")
                            .build(),
                    );
                });

                let mut service = ServiceBuilder::new()
                    .layer(SubgraphFtv1Layer::new())
                    .service(mock_service);
                let request = SubgraphRequest::fake_builder()
                    .subgraph_name(SUBGRAPH_NAME)
                    .context(Context::new())
                    .build();

                let response = service.ready().await.unwrap().call(request).await.unwrap();

                assert!(response.context.get_json_value(SUBGRAPH_FTV1).is_none());

                crate::plugin::test::await_mock_driver(driver).await;
            }
            .instrument(span)
            .await;
        }
    }
}
