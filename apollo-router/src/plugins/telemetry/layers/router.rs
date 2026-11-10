use std::collections::BTreeMap;
use std::collections::HashMap;
use std::future::ready;
use std::sync::Arc;
use std::task::Poll;
use std::time::Instant;

use ::tracing::Span;
use futures::future::BoxFuture;
use futures::future::Either;
use http::HeaderMap;
use http::HeaderName;
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
use crate::layers::release_readied;
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
use crate::plugins::telemetry::layers::Readers;
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
                ForwardHeaders::Only(only) => only.iter().any(|only| **only == *name),
                ForwardHeaders::Except(except) => !except.iter().any(|except| **except == *name),
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

/// The router layer's work whatever reads its instruments, spans and events: it refuses a request
/// whose client library headers carry a value Apollo's reports cannot hold, and writes the
/// supergraph schema's id and the client's name, version and library into the request's context,
/// which usage reporting, the selectors of instruments, events and spans, and the router's other
/// plugins read.
#[derive(Clone)]
struct ClientEntriesLayer {
    config: Arc<config::Conf>,
    supergraph_schema_id: Arc<String>,
}

impl<S> tower::Layer<S> for ClientEntriesLayer {
    type Service = ClientEntries<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ClientEntries {
            inner,
            config: self.config.clone(),
            supergraph_schema_id: self.supergraph_schema_id.clone(),
        }
    }
}

/// The service [`ClientEntriesLayer`] makes.
#[derive(Clone)]
struct ClientEntries<S> {
    inner: S,
    config: Arc<config::Conf>,
    supergraph_schema_id: Arc<String>,
}

impl<S> tower::Service<router::Request> for ClientEntries<S>
where
    S: tower::Service<router::Request, Response = router::Response, Error = BoxError> + Clone,
{
    type Response = router::Response;
    type Error = BoxError;
    /// A refusal, rare, is boxed, so that the future of a request it lets through is its inner
    /// service's.
    type Future = Either<BoxFuture<'static, Result<router::Response, BoxError>>, S::Future>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), BoxError>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: router::Request) -> Self::Future {
        let apollo = &self.config.apollo;
        let headers = request.router_request.headers();
        let valid = |header: &HeaderName| {
            headers
                .get(header)
                .and_then(|v| v.to_str().ok())
                .is_none_or(is_valid_client_library_value)
        };
        let library_name_valid = valid(&apollo.library_name_header);
        let library_version_valid = valid(&apollo.library_version_header);
        if !library_name_valid || !library_version_valid {
            // answered without the service beneath, which may hold what its readiness reserved
            release_readied(&mut self.inner);
            if !library_name_valid {
                ::tracing::warn!("Rejecting request: invalid client library name header value");
            }
            if !library_version_valid {
                ::tracing::warn!("Rejecting request: invalid client library version header value");
            }
            return Either::Left(Box::pin(ready(
                router::Response::error_builder()
                    .status_code(StatusCode::BAD_REQUEST)
                    .context(request.context)
                    .build(),
            )));
        }

        let _ = request.context.insert(
            SUPERGRAPH_SCHEMA_ID_CONTEXT_KEY,
            self.supergraph_schema_id.clone(),
        );
        let entries = [
            (&apollo.client_name_header, CLIENT_NAME),
            (&apollo.client_version_header, CLIENT_VERSION),
            (&apollo.library_name_header, CLIENT_LIBRARY_NAME),
            (&apollo.library_version_header, CLIENT_LIBRARY_VERSION),
        ];
        for (header, key) in entries {
            if let Some(value) = headers.get(&**header).and_then(|h| h.to_str().ok()) {
                let _ = request.context.insert(key, value.to_owned());
            }
        }

        Either::Right(self.inner.call(request))
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
    readers: Readers,
}

impl InstrumentRouterLayer {
    fn new(
        config: Arc<config::Conf>,
        supergraph_schema_id: Arc<String>,
        enabled_features: EnabledFeatures,
        field_level_instrumentation_ratio: f64,
        metrics_sender: apollo_exporter::Sender,
        static_router_instruments: Arc<HashMap<String, StaticInstrument>>,
        readers: Readers,
    ) -> Self {
        Self {
            config,
            readers,
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

    /// The router span where spans are built; the client's entries ([`ClientEntriesLayer`]),
    /// always; and the request's instruments, events, span attributes, usage report of an
    /// invalid operation and error counts, each where it is read. Where none of them is read the
    /// stack is the client's entries alone.
    fn layer(&self, service: S) -> Self::Service {
        let readers = self.readers;
        let client_entries = ClientEntriesLayer {
            config: self.config.clone(),
            supergraph_schema_id: self.supergraph_schema_id.clone(),
        };
        if !readers.any() {
            return ServiceBuilder::new()
                .layer(client_entries)
                .service(service)
                .boxed_clone();
        }
        let config_later = self.config.clone();
        let config_request = self.config.clone();
        let enabled_features = self.enabled_features.clone();
        let field_level_instrumentation_ratio = self.field_level_instrumentation_ratio;
        let metrics_sender = self.metrics_sender.clone();
        // the instrument set, built once for the pipeline and started per request, where a reader
        // reads the metrics
        let instrument_set = readers.metrics.then(|| {
            Arc::new(
                self.config
                    .instrumentation
                    .instruments
                    .router_instrument_set(&self.static_router_instruments),
            )
        });

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
            .layer(client_entries)
            .map_future_with_request_data(
                move |request: &router::Request| {
                    // the router span's attributes, made only where the span is built
                    let custom_attributes = if Span::current().is_disabled() {
                        Vec::new()
                    } else {
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
                        custom_attributes
                    };

                    // the router overhead tracker, in the context where something reads it
                    if readers.router_overhead() {
                        request.context.extensions().with_lock(|lock| {
                            lock.insert(router_overhead::RouterOverheadTracker::new());
                        });
                    }

                    let custom_instruments: Option<RouterInstruments> =
                        instrument_set.as_ref().map(|set| set.start());
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
                    Option<RouterInstruments>,
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

                    if readers.metrics {
                        plugin_metrics(&config);
                    }

                    async move {
                        if let Some(http_server_response_body_size) =
                            custom_instruments.as_ref().and_then(|instruments| {
                                instruments.http_server_response_body_size.as_ref()
                            })
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
                        if !span.is_disabled() {
                            let get_from_context =
                                |ctx: &Context, key| ctx.get::<&str, String>(key).ok().flatten();
                            if let Some(key) = client_name_key {
                                span.set_span_dyn_attribute(
                                    key,
                                    opentelemetry::Value::String(
                                        get_from_context(&ctx, CLIENT_NAME)
                                            .unwrap_or_default()
                                            .into(),
                                    ),
                                );
                            }
                            if let Some(key) = client_version_key {
                                span.set_span_dyn_attribute(
                                    key,
                                    opentelemetry::Value::String(
                                        get_from_context(&ctx, CLIENT_VERSION)
                                            .unwrap_or_default()
                                            .into(),
                                    ),
                                );
                            }
                        }

                        span.record(
                            APOLLO_PRIVATE_DURATION_NS,
                            start.elapsed().as_nanos() as i64,
                        );

                        let expose_trace_id = &config.exporters.tracing.response_trace_id;
                        if let Ok(response) = &response {
                            span.set_span_dyn_attributes_from(|| {
                                config
                                    .instrumentation
                                    .spans
                                    .router
                                    .attributes
                                    .on_response(response)
                            });
                            custom_instruments.on_response(response);
                            custom_events.on_response(response);

                            // the router span's response headers, recorded only where the span is
                            // built
                            if !span.is_disabled() {
                                let mut headers: HashMap<String, Vec<String>> =
                                    HashMap::with_capacity(2);
                                if expose_trace_id.enabled {
                                    let header_name = expose_trace_id
                                        .header_name
                                        .as_deref()
                                        .unwrap_or(&DEFAULT_EXPOSE_TRACE_ID_HEADER_NAME);

                                    if let Some(value) =
                                        response.response.headers().get(header_name)
                                    {
                                        headers.insert(
                                            header_name.to_string(),
                                            vec![value.to_str().unwrap_or_default().to_string()],
                                        );
                                    }
                                }
                                if let Some(value) = response.response.headers().get(&CACHE_CONTROL)
                                {
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
                            }

                            if readers.reporting
                                && response.context.extensions().with_lock(|lock| {
                                    lock.get::<Arc<UsageReporting>>()
                                        .map(|u| matches!(**u, UsageReporting::Error { .. }))
                                        .unwrap_or(false)
                                })
                            {
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
                            span.set_span_dyn_attributes_from(|| {
                                config
                                    .instrumentation
                                    .spans
                                    .router
                                    .attributes
                                    .on_error(err, &ctx)
                            });
                            custom_instruments.on_error(err, &ctx);
                            custom_events.on_error(err, &ctx);
                        }

                        match response {
                            Ok(resp) if readers.errors() => {
                                Ok(count_router_errors(resp, &config.apollo.errors).await)
                            }
                            response => response,
                        }
                    }
                },
            )
            .service(service)
            .boxed_clone()
    }
}

impl Telemetry {
    /// Returns a layer that emits per-request memory allocation metrics, where a reader reads
    /// the metrics: it tracks every allocation the request makes.
    pub(crate) fn allocation_metrics_layer(&self) -> Option<AllocationMetricsLayer> {
        self.readers().metrics.then(AllocationMetricsLayer::new)
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
            self.readers(),
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

    /// A request through the router instrument layer builds no instrument set: the layer builds
    /// the router's instruments, their conditions and their selectors once for its pipeline, and
    /// a request starts the state its measurements record into. Read as the bytes the layer
    /// allocates per request under a custom histogram whose value is a request header named at a
    /// width of 8 and of 2,056 bytes: a request that builds the custom instruments copies the
    /// selector, and the header's name with it, and one that starts them shares it.
    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    #[test]
    fn a_request_builds_no_router_instrument_set() {
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
        let bytes = |width: usize| {
            let config = serde_json::json!({"telemetry": {"instrumentation": {"instruments": {
                "router": {
                    "acme.request.header": {
                        "description": "a request header's value",
                        "type": "histogram",
                        "unit": "unit",
                        "value": {"request_header": "h".repeat(width)}
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
                .layer(crate::plugins::telemetry::layers::where_metrics_are_read(
                    || test_harness.instrument_router_layer(),
                ))
                .service(tower::service_fn(|request: router::Request| {
                    std::future::ready(
                        router::Response::fake_builder()
                            .context(request.context)
                            .data(json!({"data": {}}))
                            .build(),
                    )
                }));
            crate::allocator::bytes_per_call(
                layered,
                || router::Request::fake_builder().build().expect("a request"),
                drop,
                REQUESTS,
            )
        };

        let narrow = bytes(NARROW);
        let wide = bytes(WIDE);
        eprintln!(
            "a request allocates {narrow} bytes at a header name of {NARROW} bytes and {wide} at {WIDE}"
        );
        assert!(
            wide.saturating_sub(narrow) < WIDE - NARROW,
            "a request allocates {wide} bytes at a header name of {WIDE} bytes and {narrow} at \
             {NARROW}: a request builds the custom instruments"
        );
    }

    /// The telemetry plugin `configuration` makes, its `telemetry` section read as the router
    /// reads a plugin's configuration, with no schema to check it against: the schema leaves out
    /// the Apollo key and graph reference, which the router takes from its environment.
    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    async fn telemetry_plugin(configuration: serde_json::Value) -> Telemetry {
        let init = crate::plugin::PluginInit::fake_builder()
            .config(configuration["telemetry"].clone())
            .full_config(configuration)
            .build()
            .with_deserialized_config()
            .expect("a telemetry configuration");
        <Telemetry as crate::plugin::PluginPrivate>::new(init)
            .await
            .expect("a telemetry plugin")
    }

    /// The router instrument layer evaluates its instruments where a reader reads the router's
    /// metrics and nowhere else. The readers are the meter providers': a Prometheus or an OTLP
    /// exporter on the public provider, and Apollo's metrics with an Apollo key and graph
    /// reference; tracing, logging, an event and an Apollo key with no graph reference read no
    /// measurement. Each configuration carries a probe, a counter whose attribute is a request
    /// header's value, named under `apollo.router.operations` so that Apollo's provider, which
    /// reads no other names, reads it too. The configurations are activated one after another
    /// over one meter provider with no reader to start with, as a router reloads them, each
    /// reader followed by a configuration nothing reads metrics under, so that the evaluation
    /// stops when a reload takes the reader away; Apollo's reader comes last, since a reload that
    /// drops the key builds no Apollo provider in the place of the one it had, which keeps
    /// reading. Read as the bytes a request allocates through the layer at the header's value of
    /// 8 and of 2,056 bytes: an evaluated instrument makes the value, 2,048 bytes more at the
    /// longer.
    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    #[test]
    fn the_router_evaluates_its_instruments_only_where_a_reader_reads_them() {
        use crate::plugin::PluginPrivate as _;

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
        let source = tower::service_fn(|request: router::Request| {
            std::future::ready(
                router::Response::fake_builder()
                    .context(request.context)
                    .data(json!({"data": {}}))
                    .build(),
            )
        });
        let probed = |telemetry: &serde_json::Value| {
            let mut configuration = serde_json::json!({ "telemetry": telemetry });
            // Apollo's endpoints local, so that nothing a configuration with an Apollo key
            // exports leaves the machine
            configuration["telemetry"]["apollo"]["endpoint"] =
                "http://127.0.0.1:1/api/ingress/traces".into();
            configuration["telemetry"]["apollo"]["otlp_endpoint"] = "http://127.0.0.1:1".into();
            configuration["telemetry"]["instrumentation"]["instruments"]["router"]["apollo.router.operations.probe"] = serde_json::json!({
                "description": "requests",
                "type": "counter",
                "unit": "request",
                "value": "unit",
                "attributes": {"probe": {"request_header": "x-probe"}}
            });
            configuration
        };
        let evaluates = |telemetry: &serde_json::Value| {
            let layer = runtime.block_on(async {
                let plugin = telemetry_plugin(probed(telemetry)).await;
                plugin.activate();
                plugin.instrument_router_layer()
            });
            let bytes = |width: usize| {
                crate::allocator::bytes_per_call(
                    ServiceBuilder::new().layer(layer.clone()).service(source),
                    || {
                        router::Request::fake_builder()
                            .header("x-probe", "p".repeat(width))
                            .build()
                            .expect("a request")
                    },
                    drop,
                    REQUESTS,
                )
            };
            let (narrow, wide) = (bytes(NARROW), bytes(WIDE));
            eprintln!(
                "{telemetry}: a request allocates {narrow} bytes at a header value of {NARROW} bytes and {wide} at {WIDE}"
            );
            wide.saturating_sub(narrow) >= WIDE - NARROW
        };
        let unread = [
            ("the router's defaults", serde_json::json!({})),
            (
                "stdout logging off",
                serde_json::json!({"exporters": {"logging": {"stdout": {"enabled": false}}}}),
            ),
            (
                "a Prometheus exporter configured and disabled",
                serde_json::json!({"exporters": {"metrics": {"prometheus": {
                    "enabled": false, "listen": "127.0.0.1:9090", "path": "/metrics"
                }}}}),
            ),
            (
                "an OTLP metrics exporter configured and disabled",
                serde_json::json!({"exporters": {"metrics": {"otlp": {
                    "enabled": false, "endpoint": "http://127.0.0.1:4317"
                }}}}),
            ),
            (
                "an OTLP trace exporter, which reads spans",
                serde_json::json!({"exporters": {"tracing": {"otlp": {
                    "enabled": true, "endpoint": "http://127.0.0.1:4317"
                }}}}),
            ),
            (
                "a logged event",
                serde_json::json!({"instrumentation": {"events": {"router": {"acme.event": {
                    "message": "a request", "on": "request", "level": "info"
                }}}}}),
            ),
            (
                "an Apollo key with no graph reference",
                serde_json::json!({"apollo": {"apollo_key": "key"}}),
            ),
        ];
        let readers = [
            (
                "a Prometheus exporter",
                serde_json::json!({"exporters": {"metrics": {"prometheus": {
                    "enabled": true, "listen": "127.0.0.1:9090", "path": "/metrics"
                }}}}),
            ),
            (
                "an OTLP metrics exporter",
                serde_json::json!({"exporters": {"metrics": {"otlp": {
                    "enabled": true, "endpoint": "http://127.0.0.1:4317"
                }}}}),
            ),
            (
                "Apollo's metrics, with an Apollo key and graph reference",
                serde_json::json!({"apollo": {
                    "apollo_key": "key", "apollo_graph_ref": "graph@current"
                }}),
            ),
        ];

        // one meter provider with no reader, as a router has before its first configuration
        let provider = (
            crate::metrics::aggregation::AggregateMeterProvider::default(),
            crate::metrics::test_utils::ClonableManualReader::default(),
        );
        crate::metrics::test_utils::AGGREGATE_METER_PROVIDER_ASYNC.sync_scope(
            std::sync::OnceLock::from(provider),
            || {
                for (configuration, telemetry) in &unread {
                    assert!(
                        !evaluates(telemetry),
                        "nothing reads metrics under {configuration}, yet a request evaluates the \
                         router's instruments"
                    );
                }
                for (index, (reader, telemetry)) in readers.iter().enumerate() {
                    assert!(
                        evaluates(telemetry),
                        "{reader} reads the metrics, yet a request evaluates no instrument"
                    );
                    if index + 1 == readers.len() {
                        break;
                    }
                    let (configuration, telemetry) = &unread[index % unread.len()];
                    assert!(
                        !evaluates(telemetry),
                        "nothing reads metrics under {configuration}, yet a request evaluates the \
                         router's instruments after {reader}"
                    );
                }
            },
        );
    }

    /// Where nothing reads the telemetry layers' work, a request through the router instrument
    /// layer evaluates no instrument and allocates no attribute: the layer does only what other
    /// readers than its instruments, spans and events rely on, its check of the client library's
    /// headers and the client's and the schema's entries in the request's context. Read as the
    /// bytes the layer allocates per request beyond the source beneath it, under a custom counter
    /// whose attribute is a request header's value, at that value's width of 8 and of 2,056
    /// bytes: an evaluated instrument makes the value, 2,048 bytes more at the longer; and at the
    /// shorter against an allowance for the context's entries and the request's boxed future.
    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    #[test]
    fn a_request_nothing_reads_evaluates_no_instrument() {
        const REQUESTS: usize = 16;
        const NARROW: usize = 8;
        const WIDE: usize = 2056;
        const ALLOWANCE: usize = 1024;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime");
        let _entered = runtime.enter();
        let plugin = crate::plugins::telemetry::layers::activated_under_the_routers_subscriber(
            &runtime,
            &crate::plugins::telemetry::layers::nothing_reads(serde_json::json!({"router": {
                "acme.probe": {
                    "description": "requests",
                    "type": "counter",
                    "unit": "request",
                    "value": "unit",
                    "attributes": {"probe": {"request_header": "x-probe"}}
                }
            }})),
        );
        let source = tower::service_fn(|request: router::Request| {
            std::future::ready(
                router::Response::fake_builder()
                    .context(request.context)
                    .data(json!({"data": {}}))
                    .build(),
            )
        });
        let request = |width: usize| {
            move || {
                router::Request::fake_builder()
                    .header("x-probe", "p".repeat(width))
                    .build()
                    .expect("a request")
            }
        };
        let bytes = |width: usize| {
            let beneath = crate::allocator::bytes_per_call(source, request(width), drop, REQUESTS);
            let through = crate::allocator::bytes_per_call(
                ServiceBuilder::new()
                    .layer(plugin.instrument_router_layer())
                    .service(source),
                request(width),
                drop,
                REQUESTS,
            );
            through.saturating_sub(beneath)
        };

        let (narrow, wide) = (bytes(NARROW), bytes(WIDE));
        eprintln!(
            "the layer allocates {narrow} bytes per request beyond its source at a header value of {NARROW} bytes and {wide} at {WIDE}"
        );
        assert!(
            wide.saturating_sub(narrow) < WIDE - NARROW,
            "the layer allocates {wide} bytes per request at a header value of {WIDE} bytes and \
             {narrow} at {NARROW}: a request evaluates an instrument nothing reads"
        );
        assert!(
            narrow <= ALLOWANCE,
            "the layer allocates {narrow} bytes per request beyond its source, more than the \
             allowance of {ALLOWANCE}: a request does work nothing reads"
        );
    }

    #[test]
    fn it_test_send_headers_to_studio() {
        let fw_headers = ForwardHeaders::Only(vec![
            HeaderName::from_static("test").into(),
            HeaderName::from_static("apollo-x-name").into(),
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
