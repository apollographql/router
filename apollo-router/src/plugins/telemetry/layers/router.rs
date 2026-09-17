use std::collections::BTreeMap;
use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Instant;

use ::tracing::Span;
use http::HeaderMap;
use http::StatusCode;
use http::header::CACHE_CONTROL;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::Context;
use crate::apollo_studio_interop::UsageReporting;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::layers::ServiceBuilderExt;
use crate::layers::instrument::InstrumentLayer;
use crate::plugins::telemetry::CLIENT_LIBRARY_NAME;
use crate::plugins::telemetry::CLIENT_LIBRARY_VERSION;
use crate::plugins::telemetry::CLIENT_NAME;
use crate::plugins::telemetry::CLIENT_VERSION;
use crate::plugins::telemetry::DEFAULT_EXPOSE_TRACE_ID_HEADER_NAME;
use crate::plugins::telemetry::EnabledFeatures;
use crate::plugins::telemetry::Telemetry;
use crate::plugins::telemetry::apollo::ForwardHeaders;
use crate::plugins::telemetry::apollo_exporter;
use crate::plugins::telemetry::config;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::instruments::CustomHistogramInner;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::instruments::StaticInstrument;
use crate::plugins::telemetry::config_new::router::events::RouterEvents;
use crate::plugins::telemetry::config_new::router::instruments::ResponseBodySizeRecording;
use crate::plugins::telemetry::config_new::router::instruments::RouterInstruments;
use crate::plugins::telemetry::config_new::router_overhead;
use crate::plugins::telemetry::consts::OTEL_NAME;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_ERROR;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_OK;
use crate::plugins::telemetry::consts::ROUTER_SPAN_NAME;
use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;
use crate::plugins::telemetry::error_counter::count_router_errors;
use crate::plugins::telemetry::is_valid_client_library_value;
use crate::plugins::telemetry::metrics::allocation::AllocationMetricsLayer;
use crate::plugins::telemetry::reload::metrics::MetricsConfigurator;
use crate::plugins::telemetry::reload::tracing::TracingConfigurator;
use crate::plugins::telemetry::span_factory;
use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_DURATION_NS;
use crate::plugins::telemetry::tracing::apollo_telemetry::CLIENT_NAME_KEY;
use crate::plugins::telemetry::tracing::apollo_telemetry::CLIENT_VERSION_KEY;
use crate::query_planner::OperationKind;
use crate::services::router;

const SUPERGRAPH_SCHEMA_ID_CONTEXT_KEY: &str = "apollo::supergraph_schema_id";

fn filter_headers(
    headers: &HeaderMap,
    forward_rules: &ForwardHeaders,
    context: &Context,
) -> String {
    if let ForwardHeaders::None = forward_rules {
        return String::from("{}");
    }
    let headers_map = headers
        .iter()
        .filter(|(name, _value)| {
            // Never forward sensitive headers to Apollo trace exports. Sensitivity
            // is governed by the shared header-masking config (with the built-in
            // fail-secure defaults — authorization, cookie, set-cookie, … — when
            // unconfigured), so a user-configured sensitive header is redacted here
            // too, rather than only the legacy hardcoded set.
            !crate::services::header_masking::is_sensitive_request_header(context, name.as_str())
        })
        .filter_map(|(name, value)| {
            let send_header = match &forward_rules {
                ForwardHeaders::None => false,
                ForwardHeaders::All => true,
                ForwardHeaders::Only(only) => only.contains(name),
                ForwardHeaders::Except(except) => !except.contains(name),
            };

            send_header.then(|| {
                (
                    name.to_string(),
                    value.to_str().unwrap_or("<unknown>").to_string(),
                )
            })
        })
        .fold(
            BTreeMap::new(),
            |mut acc: BTreeMap<String, Vec<String>>, (name, value)| {
                acc.entry(name).or_default().push(value);
                acc
            },
        );

    match serde_json::to_string(&headers_map) {
        Ok(result) => result,
        Err(_err) => {
            ::tracing::warn!("could not serialize header, trace will not have header information");
            Default::default()
        }
    }
}

fn plugin_metrics(config: &Arc<Conf>) {
    let mut attributes = Vec::new();
    if MetricsConfigurator::is_enabled(&config.exporters.metrics.otlp) {
        attributes.push(KeyValue::new("telemetry.metrics.otlp", true));
    }
    if config.exporters.metrics.prometheus.enabled {
        attributes.push(KeyValue::new("telemetry.metrics.prometheus", true));
    }
    if TracingConfigurator::is_enabled(&config.exporters.tracing.otlp) {
        attributes.push(KeyValue::new("telemetry.tracing.otlp", true));
    }
    if config.exporters.tracing.datadog.is_enabled() {
        attributes.push(KeyValue::new("telemetry.tracing.datadog", true));
    }

    if !attributes.is_empty() {
        u64_counter!(
            "apollo.router.operations.telemetry",
            "Telemetry exporters enabled",
            1,
            attributes
        );
    }
}

/// Layer type for [Telemetry::instrument_router_layer].
#[derive(Clone)]
pub(crate) struct InstrumentRouterLayer {
    config: Arc<config::Conf>,
    supergraph_schema_id: Arc<String>,
    enabled_features: EnabledFeatures,
    field_level_instrumentation_ratio: f64,
    metrics_sender: apollo_exporter::Sender,
    static_router_instruments: Arc<HashMap<String, StaticInstrument>>,
}

impl InstrumentRouterLayer {
    fn new(
        config: Arc<config::Conf>,
        supergraph_schema_id: Arc<String>,
        enabled_features: EnabledFeatures,
        field_level_instrumentation_ratio: f64,
        metrics_sender: apollo_exporter::Sender,
        static_router_instruments: Arc<HashMap<String, StaticInstrument>>,
    ) -> Self {
        Self {
            config,
            supergraph_schema_id,
            enabled_features,
            field_level_instrumentation_ratio,
            metrics_sender,
            static_router_instruments,
        }
    }
}

impl<S> tower::Layer<S> for InstrumentRouterLayer
where
    S: tower::Service<router::Request, Response = router::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = router::BoxCloneService;

    fn layer(&self, service: S) -> Self::Service {
        let supergraph_schema_id = self.supergraph_schema_id.clone();
        let config_later = self.config.clone();
        let config_request = self.config.clone();
        let config_checkpoint = self.config.clone();
        let enabled_features = self.enabled_features.clone();
        let field_level_instrumentation_ratio = self.field_level_instrumentation_ratio;
        let metrics_sender = self.metrics_sender.clone();
        let static_router_instruments = self.static_router_instruments.clone();

        let spans = &self.config.instrumentation.spans;
        let router_attributes = &spans.router.attributes.attributes;

        let client_name_key = router_attributes
            .client_name
            .as_ref()
            .and_then(|a| a.key(CLIENT_NAME_KEY));

        let client_version_key = router_attributes
            .client_version
            .as_ref()
            .and_then(|a| a.key(CLIENT_VERSION_KEY));

        ServiceBuilder::new()
            .map_response(move |response: router::Response| {
                // The current span *should* be the request span as we are outside the instrument block.
                let span = Span::current();
                if let Some(span_name) = span.metadata().map(|metadata| metadata.name())
                    && span_name == ROUTER_SPAN_NAME
                {
                    //https://opentelemetry.io/docs/specs/otel/trace/semantic_conventions/instrumentation/graphql/
                    let operation_kind = response.context.get::<_, String>(OPERATION_KIND);
                    let operation_name = response.context.get::<_, String>(OPERATION_NAME);

                    if let Ok(Some(operation_kind)) = &operation_kind {
                        span.record("graphql.operation.type", operation_kind);
                    }
                    if let Ok(Some(operation_name)) = &operation_name {
                        span.record("graphql.operation.name", operation_name);
                    }
                    match (&operation_kind, &operation_name) {
                        (Ok(Some(kind)), Ok(Some(name))) => span.set_span_dyn_attribute(
                            OTEL_NAME.into(),
                            format!("{kind} {name}").into(),
                        ),
                        (Ok(Some(kind)), _) => {
                            span.set_span_dyn_attribute(OTEL_NAME.into(), kind.clone().into())
                        }
                        _ => span
                            .set_span_dyn_attribute(OTEL_NAME.into(), "GraphQL Operation".into()),
                    };
                }

                response
            })
            .layer(InstrumentLayer::new(move |request: &router::Request| {
                // When running through axum, the TraceLayer holds a "router" span guard
                // across the entire synchronous call chain, so Span::current() already
                // returns it here — reuse it rather than creating a duplicate SERVER span.
                // In tests that bypass axum, there is no active span, so we create one to
                // match the behavior users actually see.
                let current = Span::current();
                if current
                    .metadata()
                    .is_some_and(|m| m.name() == ROUTER_SPAN_NAME)
                {
                    current
                } else {
                    span_factory::create_router(&request.router_request)
                }
            }))
            .checkpoint_async(move |req: router::Request| {
                let config_checkpoint = config_checkpoint.clone();
                async move {
                    let library_name_valid = req
                        .router_request
                        .headers()
                        .get(&config_checkpoint.apollo.library_name_header)
                        .and_then(|v| v.to_str().ok())
                        .is_none_or(is_valid_client_library_value);
                    let library_version_valid = req
                        .router_request
                        .headers()
                        .get(&config_checkpoint.apollo.library_version_header)
                        .and_then(|v| v.to_str().ok())
                        .is_none_or(is_valid_client_library_value);
                    if !library_name_valid || !library_version_valid {
                        if !library_name_valid {
                            ::tracing::warn!(
                                "Rejecting request: invalid client library name header value"
                            );
                        }
                        if !library_version_valid {
                            ::tracing::warn!(
                                "Rejecting request: invalid client library version header value"
                            );
                        }
                        Ok(ControlFlow::Break(
                            router::Response::error_builder()
                                .status_code(StatusCode::BAD_REQUEST)
                                .context(req.context)
                                .build()?,
                        ))
                    } else {
                        Ok(ControlFlow::Continue(req))
                    }
                }
            })
            .map_future_with_request_data(
                move |request: &router::Request| {
                    let _ = request.context.insert(
                        SUPERGRAPH_SCHEMA_ID_CONTEXT_KEY,
                        supergraph_schema_id.clone(),
                    );

                    let client_name = request
                        .router_request
                        .headers()
                        .get(&config_request.apollo.client_name_header)
                        .and_then(|h| h.to_str().ok());
                    let client_version = request
                        .router_request
                        .headers()
                        .get(&config_request.apollo.client_version_header)
                        .and_then(|h| h.to_str().ok());

                    if let Some(name) = client_name {
                        let _ = request.context.insert(CLIENT_NAME, name.to_owned());
                    }

                    if let Some(version) = client_version {
                        let _ = request.context.insert(CLIENT_VERSION, version.to_owned());
                    }

                    let library_name = request
                        .router_request
                        .headers()
                        .get(&config_request.apollo.library_name_header)
                        .and_then(|h| h.to_str().ok());
                    let library_version = request
                        .router_request
                        .headers()
                        .get(&config_request.apollo.library_version_header)
                        .and_then(|h| h.to_str().ok());

                    if let Some(name) = library_name {
                        let _ = request.context.insert(CLIENT_LIBRARY_NAME, name.to_owned());
                    }

                    if let Some(version) = library_version {
                        let _ = request
                            .context
                            .insert(CLIENT_LIBRARY_VERSION, version.to_owned());
                    }

                    let mut custom_attributes = config_request
                        .instrumentation
                        .spans
                        .router
                        .attributes
                        .on_request(request);

                    custom_attributes.push(KeyValue::new(
                        Key::from_static_str("apollo_private.http.request_headers"),
                        filter_headers(
                            request.router_request.headers(),
                            &config_request.apollo.send_headers,
                            &request.context,
                        ),
                    ));

                    // Create and store router overhead tracker in context
                    request.context.extensions().with_lock(|lock| {
                        lock.insert(router_overhead::RouterOverheadTracker::new());
                    });

                    let custom_instruments: RouterInstruments = config_request
                        .instrumentation
                        .instruments
                        .new_router_instruments(static_router_instruments.clone());
                    custom_instruments.on_request(request);

                    let mut custom_events: RouterEvents =
                        config_request.instrumentation.events.new_router_events();
                    custom_events.on_request(request);

                    (
                        custom_attributes,
                        custom_instruments,
                        custom_events,
                        request.context.clone(),
                    )
                },
                move |(custom_attributes, custom_instruments, mut custom_events, ctx): (
                    Vec<KeyValue>,
                    RouterInstruments,
                    RouterEvents,
                    Context,
                ),
                      fut| {
                    let start = Instant::now();
                    let config = config_later.clone();
                    let sender = metrics_sender.clone();
                    let enabled_features = enabled_features.clone();
                    let client_name_key = client_name_key.clone();
                    let client_version_key = client_version_key.clone();

                    plugin_metrics(&config);

                    async move {
                        if let Some(http_server_response_body_size) =
                            &custom_instruments.http_server_response_body_size
                        {
                            let CustomHistogramInner {
                                histogram,
                                attributes,
                                ..
                            } = &*http_server_response_body_size.inner.lock();
                            // Clone the histogram (which uses an Arc internally) and store
                            // in ResponseBodySizeRecording so that we can later record the
                            // final byte count after the body stream is fully sent.
                            if let Some(histogram) = &histogram {
                                let recording = ResponseBodySizeRecording::new(
                                    histogram.clone(),
                                    attributes.clone(),
                                );
                                ctx.extensions().with_lock(|lock| lock.insert(recording));
                            }
                        }

                        let span = Span::current();
                        span.set_span_dyn_attributes(custom_attributes);
                        let response: Result<router::Response, BoxError> = fut.await;

                        // Client name and version must be picked up after awaiting
                        // the inner service future, because router service plugins
                        // (e.g. rhai) may modify these values in the shared context
                        // during request processing. With buffered service layers,
                        // that processing is deferred until the future is polled.
                        let get_from_context =
                            |ctx: &Context, key| ctx.get::<&str, String>(key).ok().flatten();
                        let client_name = get_from_context(&ctx, CLIENT_NAME);
                        let client_version = get_from_context(&ctx, CLIENT_VERSION);

                        if let Some(key) = client_name_key {
                            span.set_span_dyn_attribute(
                                key,
                                opentelemetry::Value::String(
                                    client_name.unwrap_or_default().into(),
                                ),
                            );
                        }

                        if let Some(key) = client_version_key {
                            span.set_span_dyn_attribute(
                                key,
                                opentelemetry::Value::String(
                                    client_version.unwrap_or_default().into(),
                                ),
                            );
                        }

                        span.record(
                            APOLLO_PRIVATE_DURATION_NS,
                            start.elapsed().as_nanos() as i64,
                        );

                        let expose_trace_id = &config.exporters.tracing.response_trace_id;
                        if let Ok(response) = &response {
                            span.set_span_dyn_attributes(
                                config
                                    .instrumentation
                                    .spans
                                    .router
                                    .attributes
                                    .on_response(response),
                            );
                            custom_instruments.on_response(response);
                            custom_events.on_response(response);

                            let mut headers: HashMap<String, Vec<String>> =
                                HashMap::with_capacity(2);
                            if expose_trace_id.enabled {
                                let header_name = expose_trace_id
                                    .header_name
                                    .as_ref()
                                    .unwrap_or(&DEFAULT_EXPOSE_TRACE_ID_HEADER_NAME);

                                if let Some(value) = response.response.headers().get(header_name) {
                                    headers.insert(
                                        header_name.to_string(),
                                        vec![value.to_str().unwrap_or_default().to_string()],
                                    );
                                }
                            }
                            if let Some(value) = response.response.headers().get(&CACHE_CONTROL) {
                                headers.insert(
                                    CACHE_CONTROL.to_string(),
                                    vec![value.to_str().unwrap_or_default().to_string()],
                                );
                            }
                            if !headers.is_empty() {
                                let response_headers =
                                    serde_json::to_string(&headers).unwrap_or_default();
                                span.record(
                                    "apollo_private.http.response_headers",
                                    &response_headers,
                                );
                            }

                            if response.context.extensions().with_lock(|lock| {
                                lock.get::<Arc<UsageReporting>>()
                                    .map(|u| matches!(**u, UsageReporting::Error { .. }))
                                    .unwrap_or(false)
                            }) {
                                Telemetry::update_apollo_metrics(
                                    &response.context,
                                    field_level_instrumentation_ratio,
                                    sender,
                                    true,
                                    start.elapsed(),
                                    // the query is invalid, we did not parse the operation kind
                                    OperationKind::Query,
                                    None,
                                    Default::default(),
                                    enabled_features.clone(),
                                );
                            }

                            if response.response.status() >= StatusCode::BAD_REQUEST {
                                span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                            } else {
                                span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_OK);
                            }
                        } else if let Err(err) = &response {
                            span.record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                            span.set_span_dyn_attributes(
                                config
                                    .instrumentation
                                    .spans
                                    .router
                                    .attributes
                                    .on_error(err, &ctx),
                            );
                            custom_instruments.on_error(err, &ctx);
                            custom_events.on_error(err, &ctx);
                        }

                        if let Ok(resp) = response {
                            Ok(count_router_errors(resp, &config.apollo.errors).await)
                        } else {
                            response
                        }
                    }
                },
            )
            .service(service)
            .boxed_clone()
    }
}

impl Telemetry {
    /// Returns a layer that emits per-request memory allocation metrics.
    pub(crate) fn allocation_metrics_layer(&self) -> AllocationMetricsLayer {
        AllocationMetricsLayer::new()
    }

    /// Returns a layer that instruments the router service with Apollo and custom
    /// instrumentation.
    pub(crate) fn instrument_router_layer(&self) -> InstrumentRouterLayer {
        let static_router_instruments = self
            .builtin_instruments
            .read()
            .router_custom_instruments
            .clone();
        InstrumentRouterLayer::new(
            self.config.clone(),
            self.supergraph_schema_id.clone(),
            self.enabled_features.clone(),
            self.field_level_instrumentation_ratio,
            self.apollo_metrics_sender.clone(),
            static_router_instruments,
        )
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderName;
    use http::HeaderValue;
    use serde_json_bytes::json;
    use tower::Service as _;

    use super::*;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::test::PluginTestHarness;
    use crate::services::RouterRequest;
    use crate::services::RouterResponse;

    #[test]
    fn it_test_send_headers_to_studio() {
        let fw_headers = ForwardHeaders::Only(vec![
            HeaderName::from_static("test"),
            HeaderName::from_static("apollo-x-name"),
        ]);
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("xxx"),
        );
        headers.insert(
            HeaderName::from_static("test"),
            HeaderValue::from_static("content"),
        );
        headers.insert(
            HeaderName::from_static("referer"),
            HeaderValue::from_static("test"),
        );
        headers.insert(
            HeaderName::from_static("foo"),
            HeaderValue::from_static("bar"),
        );
        headers.insert(
            HeaderName::from_static("apollo-x-name"),
            HeaderValue::from_static("polaris"),
        );
        let filtered_headers = filter_headers(&headers, &fw_headers, &crate::Context::new());
        assert_eq!(
            filtered_headers.as_str(),
            r#"{"apollo-x-name":["polaris"],"test":["content"]}"#
        );
        let filtered_headers =
            filter_headers(&headers, &ForwardHeaders::None, &crate::Context::new());
        assert_eq!(filtered_headers.as_str(), "{}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_custom_router_instruments() {
        async {
            let plugin = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!("../testdata/custom_instruments.router.yaml"))
                .build()
                .await
                .expect("test harness");

            let (mock_bad_request_service, mut handle) =
                tower_test::mock::pair::<RouterRequest, RouterResponse>();
            let driver = tokio::spawn(async move {
                for _ in 0..2 {
                    let (req, responder) = handle.next_request().await.unwrap();
                    responder.send_response(
                        RouterResponse::fake_builder()
                            .context(req.context)
                            .status_code(StatusCode::BAD_REQUEST)
                            .header("content-type", "application/json")
                            .data(json!({"errors": [{"message": "nope"}]}))
                            .build()
                            .unwrap(),
                    );
                }
            });
            let mut bad_request_router_service = ServiceBuilder::new()
                .layer(plugin.instrument_router_layer())
                .service(mock_bad_request_service);
            let router_req = RouterRequest::fake_builder()
                .header("x-custom", "TEST")
                .header("conditional-custom", "X")
                .header("custom-length", "55")
                .header("content-length", "55")
                .header("content-type", "application/graphql");
            let _router_response = bad_request_router_service
                .ready()
                .await
                .unwrap()
                .call(router_req.build().unwrap())
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap();

            assert_counter!("acme.graphql.custom_req", 1.0);
            assert_histogram_sum!(
                "http.server.request.body.size",
                55.0,
                "http.response.status_code" = 400,
                "acme.my_attribute" = "application/json"
            );
            assert_histogram_sum!("acme.request.length", 55.0);

            let router_req = RouterRequest::fake_builder()
                .header("x-custom", "TEST")
                .header("custom-length", "5")
                .header("content-length", "5")
                .header("content-type", "application/graphql");
            let _router_response = bad_request_router_service
                .ready()
                .await
                .unwrap()
                .call(router_req.build().unwrap())
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap();
            assert_counter!("acme.graphql.custom_req", 1.0);
            assert_histogram_sum!("acme.request.length", 60.0);
            assert_histogram_sum!(
                "http.server.request.body.size",
                60.0,
                "http.response.status_code" = 400,
                "acme.my_attribute" = "application/json"
            );
            drop(bad_request_router_service);
            crate::plugin::test::await_mock_driver(driver).await;
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_custom_router_instruments_with_requirement_level() {
        async {
            let plugin = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!(
                    "../testdata/custom_instruments_level.router.yaml"
                ))
                .build()
                .await
                .expect("test harness");

            let (mock_bad_request_service, mut handle) =
                tower_test::mock::pair::<RouterRequest, RouterResponse>();
            let driver = tokio::spawn(async move {
                for _ in 0..2 {
                    let (req, responder) = handle.next_request().await.unwrap();
                    responder.send_response(
                        RouterResponse::fake_builder()
                            .context(req.context)
                            .status_code(StatusCode::BAD_REQUEST)
                            .header("content-type", "application/json")
                            .data(json!({"errors": [{"message": "nope"}]}))
                            .build()
                            .unwrap(),
                    );
                }
            });
            let mut bad_request_router_service = ServiceBuilder::new()
                .layer(plugin.instrument_router_layer())
                .service(mock_bad_request_service);
            let router_req = RouterRequest::fake_builder()
                .header("x-custom", "TEST")
                .header("conditional-custom", "X")
                .header("custom-length", "55")
                .header("content-length", "55")
                .header("content-type", "application/graphql");
            let _router_response = bad_request_router_service
                .ready()
                .await
                .unwrap()
                .call(router_req.build().unwrap())
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap();

            assert_counter!("acme.graphql.custom_req", 1.0);
            assert_histogram_sum!(
                "http.server.request.body.size",
                55.0,
                "acme.my_attribute" = "application/json",
                "error.type" = "Bad Request",
                "http.response.status_code" = 400,
                "network.protocol.version" = "HTTP/1.1"
            );
            assert_histogram_exists!(
                "http.server.request.duration",
                f64,
                "error.type" = "Bad Request",
                "http.response.status_code" = 400,
                "network.protocol.version" = "HTTP/1.1",
                "http.request.method" = "GET"
            );
            assert_histogram_sum!("acme.request.length", 55.0);

            let router_req = RouterRequest::fake_builder()
                .header("x-custom", "TEST")
                .header("custom-length", "5")
                .header("content-length", "5")
                .header("content-type", "application/graphql");
            let _router_response = bad_request_router_service
                .ready()
                .await
                .unwrap()
                .call(router_req.build().unwrap())
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap();
            assert_counter!("acme.graphql.custom_req", 1.0);
            assert_histogram_sum!("acme.request.length", 60.0);
            assert_histogram_sum!(
                "http.server.request.body.size",
                60.0,
                "http.response.status_code" = 400,
                "acme.my_attribute" = "application/json",
                "error.type" = "Bad Request",
                "network.protocol.version" = "HTTP/1.1"
            );
            drop(bad_request_router_service);
            crate::plugin::test::await_mock_driver(driver).await;
        }
        .with_metrics()
        .await;
    }
}
