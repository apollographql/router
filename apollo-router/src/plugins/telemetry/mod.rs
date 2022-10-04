//! Telemetry plugin.
// With regards to ELv2 licensing, this entire file is license key functionality
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::error::Error as Errors;
use std::fmt;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use ::tracing::field;
use ::tracing::info_span;
#[cfg(not(feature = "console"))]
use ::tracing::subscriber::set_global_default;
use ::tracing::Span;
use ::tracing::Subscriber;
use futures::future::ready;
use futures::future::BoxFuture;
use futures::stream::once;
use futures::FutureExt;
use futures::StreamExt;
use http::header;
use http::HeaderMap;
use http::HeaderValue;
use multimap::MultiMap;
use once_cell::sync::OnceCell;
use opentelemetry::global;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::sdk::propagation::BaggagePropagator;
use opentelemetry::sdk::propagation::TextMapCompositePropagator;
use opentelemetry::sdk::propagation::TraceContextPropagator;
use opentelemetry::sdk::trace::Builder;
use opentelemetry::trace::SpanKind;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TracerProvider;
use opentelemetry::KeyValue;
use rand::Rng;
use router_bridge::planner::UsageReporting;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
#[cfg(not(feature = "console"))]
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Registry;
use url::Url;

use self::apollo::ForwardValues;
use self::apollo::SingleReport;
use self::apollo_exporter::Sender;
use self::config::Conf;
use self::metrics::AttributesForwardConf;
use self::metrics::MetricsAttributesConf;
#[cfg(not(feature = "console"))]
use crate::executable::GLOBAL_ENV_FILTER;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::telemetry::apollo::ForwardHeaders;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::config::Trace;
use crate::plugins::telemetry::metrics::apollo::studio::SingleContextualizedStats;
use crate::plugins::telemetry::metrics::apollo::studio::SingleQueryLatencyStats;
use crate::plugins::telemetry::metrics::apollo::studio::SingleStats;
use crate::plugins::telemetry::metrics::apollo::studio::SingleStatsReport;
use crate::plugins::telemetry::metrics::AggregateMeterProvider;
use crate::plugins::telemetry::metrics::BasicMetrics;
use crate::plugins::telemetry::metrics::MetricsBuilder;
use crate::plugins::telemetry::metrics::MetricsConfigurator;
use crate::plugins::telemetry::metrics::MetricsExporterHandle;
use crate::plugins::telemetry::tracing::TracingConfigurator;
use crate::query_planner::USAGE_REPORTING;
use crate::register_plugin;
use crate::router_factory::Endpoint;
use crate::services::execution;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::spaceport::server::ReportSpaceport;
use crate::spaceport::StatsContext;
use crate::subgraph::Request;
use crate::subgraph::Response;
use crate::Context;
use crate::ExecutionRequest;
use crate::ListenAddr;
use crate::SubgraphRequest;
use crate::SubgraphResponse;
use crate::SupergraphRequest;
use crate::SupergraphResponse;

pub(crate) mod apollo;
pub(crate) mod apollo_exporter;
pub(crate) mod config;
pub(crate) mod formatter;
mod metrics;
mod otlp;
mod tracing;
pub(crate) const SUPERGRAPH_SPAN_NAME: &str = "supergraph";
pub(crate) const SUBGRAPH_SPAN_NAME: &str = "subgraph";
const CLIENT_NAME: &str = "apollo_telemetry::client_name";
const CLIENT_VERSION: &str = "apollo_telemetry::client_version";
const ATTRIBUTES: &str = "apollo_telemetry::metrics_attributes";
const SUBGRAPH_ATTRIBUTES: &str = "apollo_telemetry::subgraph_metrics_attributes";
pub(crate) const STUDIO_EXCLUDE: &str = "apollo_telemetry::studio::exclude";
pub(crate) const FTV1_DO_NOT_SAMPLE: &str = "apollo_telemetry::studio::ftv1_do_not_sample";
const DEFAULT_SERVICE_NAME: &str = "apollo-router";

static TELEMETRY_LOADED: OnceCell<bool> = OnceCell::new();
static TELEMETRY_REFCOUNT: AtomicU8 = AtomicU8::new(0);

#[doc(hidden)] // Only public for integration tests
pub struct Telemetry {
    config: config::Conf,
    // Do not remove _metrics_exporters. Metrics will not be exported if it is removed.
    // Typically the handles are a PushController but may be something else. Dropping the handle will
    // shutdown exporter.
    _metrics_exporters: Vec<MetricsExporterHandle>,
    meter_provider: AggregateMeterProvider,
    custom_endpoints: MultiMap<ListenAddr, Endpoint>,
    apollo_metrics_sender: apollo_exporter::Sender,
    field_level_instrumentation_ratio: f64,
}

#[derive(Debug)]
struct ReportingError;

impl fmt::Display for ReportingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ReportingError")
    }
}

impl std::error::Error for ReportingError {}

fn setup_tracing<T: TracingConfigurator>(
    mut builder: Builder,
    configurator: &Option<T>,
    tracing_config: &Trace,
) -> Result<Builder, BoxError> {
    if let Some(config) = configurator {
        builder = config.apply(builder, tracing_config)?;
    }
    Ok(builder)
}

fn setup_metrics_exporter<T: MetricsConfigurator>(
    mut builder: MetricsBuilder,
    configurator: &Option<T>,
    metrics_common: &MetricsCommon,
) -> Result<MetricsBuilder, BoxError> {
    if let Some(config) = configurator {
        builder = config.apply(builder, metrics_common)?;
    }
    Ok(builder)
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        ::tracing::debug!("dropping telemetry...");
        let count = TELEMETRY_REFCOUNT.fetch_sub(1, Ordering::Relaxed);
        if count < 2 {
            std::thread::spawn(|| {
                opentelemetry::global::shutdown_tracer_provider();
            });
        }
    }
}

#[async_trait::async_trait]
impl Plugin for Telemetry {
    type Config = config::Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Self::new_common::<Registry>(init.config, None).await
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        let metrics_sender = self.apollo_metrics_sender.clone();
        let metrics = BasicMetrics::new(&self.meter_provider);
        let config = Arc::new(self.config.clone());
        let config_map_res = config.clone();
        ServiceBuilder::new()
            .instrument(Self::supergraph_service_span(
                self.field_level_instrumentation_ratio,
                config.apollo.clone().unwrap_or_default(),
            ))
            .map_response(|resp: SupergraphResponse| {
                if let Ok(Some(usage_reporting)) =
                    resp.context.get::<_, UsageReporting>(USAGE_REPORTING)
                {
                    // Record the operation signature on the router span
                    Span::current().record(
                        "apollo_private.operation_signature",
                        &usage_reporting.stats_report_key.as_str(),
                    );
                }
                resp
            })
            .map_future_with_request_data(
                move |req: &SupergraphRequest| {
                    Self::populate_context(config.clone(), req);
                    req.context.clone()
                },
                move |ctx: Context, fut| {
                    let config = config_map_res.clone();
                    let metrics = metrics.clone();
                    let sender = metrics_sender.clone();
                    let start = Instant::now();
                    async move {
                        let mut result: Result<SupergraphResponse, BoxError> = fut.await;
                        result = Self::update_otel_metrics(
                            config.clone(),
                            ctx.clone(),
                            metrics.clone(),
                            result,
                            start.elapsed(),
                        )
                        .await;
                        Self::update_metrics_on_last_response(
                            &ctx, config, metrics, sender, start, result,
                        )
                    }
                },
            )
            .service(service)
            .boxed()
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        ServiceBuilder::new()
            .instrument(move |req: &ExecutionRequest| {
                // disable ftv1 sampling for deferred queries
                let do_not_sample_reason = if req.query_plan.root.contains_condition_or_defer() {
                    req.context.insert(FTV1_DO_NOT_SAMPLE, true).unwrap();
                    "query is deferred"
                } else {
                    ""
                };
                let query = req
                    .supergraph_request
                    .body()
                    .query
                    .clone()
                    .unwrap_or_default();
                let operation_name = req
                    .supergraph_request
                    .body()
                    .operation_name
                    .clone()
                    .unwrap_or_default();
                info_span!("execution",
                    graphql.document = query.as_str(),
                    graphql.operation.name = operation_name.as_str(),
                    "otel.kind" = %SpanKind::Internal,
                    ftv1.do_not_sample_reason = do_not_sample_reason
                )
            })
            .service(service)
            .boxed()
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        let metrics = BasicMetrics::new(&self.meter_provider);
        let subgraph_attribute = KeyValue::new("subgraph", name.to_string());
        let subgraph_metrics_conf_req = self.create_subgraph_metrics_conf(name);
        let subgraph_metrics_conf_resp = subgraph_metrics_conf_req.clone();
        let name = name.to_owned();
        let apollo_handler = self.apollo_handler();
        ServiceBuilder::new()
            .instrument(move |req: &SubgraphRequest| {
                let query = req
                    .subgraph_request
                    .body()
                    .query
                    .clone()
                    .unwrap_or_default();
                let operation_name = req
                    .subgraph_request
                    .body()
                    .operation_name
                    .clone()
                    .unwrap_or_default();

                info_span!(SUBGRAPH_SPAN_NAME,
                    "apollo.subgraph.name" = name.as_str(),
                    graphql.document = query.as_str(),
                    graphql.operation.name = operation_name.as_str(),
                    "otel.kind" = %SpanKind::Internal,
                    "apollo_private.ftv1" = field::Empty
                )
            })
            .map_request(move |req| apollo_handler.request_ftv1(req))
            .map_response(move |resp| apollo_handler.store_ftv1(resp))
            .map_future_with_request_data(
                move |sub_request: &SubgraphRequest| {
                    Self::store_subgraph_request_attributes(
                        subgraph_metrics_conf_req.clone(),
                        sub_request,
                    );
                    sub_request.context.clone()
                },
                move |context: Context,
                      f: BoxFuture<'static, Result<SubgraphResponse, BoxError>>| {
                    let metrics = metrics.clone();
                    let subgraph_attribute = subgraph_attribute.clone();
                    let subgraph_metrics_conf = subgraph_metrics_conf_resp.clone();
                    // Using Instant because it is guaranteed to be monotonically increasing.
                    let now = Instant::now();
                    f.map(move |result: Result<SubgraphResponse, BoxError>| {
                        Self::store_subgraph_response_attributes(
                            &context,
                            metrics,
                            subgraph_attribute,
                            subgraph_metrics_conf,
                            now,
                            &result,
                        );
                        result
                    })
                },
            )
            .service(service)
            .boxed()
    }

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        self.custom_endpoints.clone()
    }
}

impl Telemetry {
    /// This method can be used instead of `Plugin::new` to override the subscriber
    pub async fn new_with_subscriber<S>(
        config: serde_json::Value,
        subscriber: S,
    ) -> Result<Self, BoxError>
    where
        S: Subscriber + Send + Sync + for<'span> LookupSpan<'span>,
    {
        Self::new_common(serde_json::from_value(config)?, Some(subscriber)).await
    }

    /// This method can be used instead of `Plugin::new` to override the subscriber
    async fn new_common<S>(
        mut config: <Self as Plugin>::Config,
        #[cfg_attr(feature = "console", allow(unused_variables))] subscriber: Option<S>,
    ) -> Result<Self, BoxError>
    where
        S: Subscriber + Send + Sync + for<'span> LookupSpan<'span>,
    {
        // Apollo config is special because we enable tracing if some env variables are present.
        let apollo = config
            .apollo
            .as_mut()
            .expect("telemetry apollo config must be present");

        // If we have key and graph ref but no endpoint we start embedded spaceport
        let spaceport = match apollo {
            apollo::Config {
                apollo_key: Some(_),
                apollo_graph_ref: Some(_),
                endpoint: None,
                ..
            } => {
                ::tracing::debug!("starting Spaceport");
                let report_spaceport = ReportSpaceport::new("127.0.0.1:0".parse()?).await?;
                // Now that the port is known update the config
                apollo.endpoint = Some(Url::parse(&format!(
                    "https://{}",
                    report_spaceport.address()
                ))?);
                Some(report_spaceport)
            }
            _ => None,
        };

        // Setup metrics
        // The act of setting up metrics will overwrite a global meter. However it is essential that
        // we use the aggregate meter provider that is created below. It enables us to support
        // sending metrics to multiple providers at once, of which hopefully Apollo Studio will
        // eventually be one.
        let mut builder = Self::create_metrics_exporters(&config)?;

        // the global tracer and subscriber initialization step must be performed only once
        TELEMETRY_LOADED.get_or_try_init::<_, BoxError>(|| {
            #[cfg(not(feature = "console"))]
            use anyhow::Context;
            let tracer_provider = Self::create_tracer_provider(&config)?;

            let tracer = tracer_provider.versioned_tracer(
                "apollo-router",
                Some(env!("CARGO_PKG_VERSION")),
                None,
            );

            global::set_tracer_provider(tracer_provider);
            global::set_error_handler(handle_error)
                .expect("otel error handler lock poisoned, fatal");
            global::set_text_map_propagator(Self::create_propagator(&config));

            #[cfg(feature = "console")]
            {
                use tracing_subscriber::util::SubscriberInitExt;
                let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
                tracing_subscriber::registry()
                    .with(console_subscriber::spawn())
                    .with(tracing_subscriber::fmt::layer())
                    .with(telemetry)
                    .init();
            }

            #[cfg(not(feature = "console"))]
            {
                let log_level = GLOBAL_ENV_FILTER
                    .get()
                    .map(|s| s.as_str())
                    .unwrap_or("info");

                let sub_builder = tracing_subscriber::fmt::fmt().with_env_filter(
                    EnvFilter::try_new(log_level).context("could not parse log configuration")?,
                );

                if let Some(sub) = subscriber {
                    let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
                    let subscriber = sub.with(telemetry);
                    if let Err(e) = set_global_default(subscriber) {
                        ::tracing::error!("cannot set global subscriber: {:?}", e);
                    }
                } else if atty::is(atty::Stream::Stdout) {
                    let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

                    let subscriber = sub_builder
                        .event_format(formatter::TextFormatter::new())
                        .finish()
                        .with(telemetry);
                    if let Err(e) = set_global_default(subscriber) {
                        ::tracing::error!("cannot set global subscriber: {:?}", e);
                    }
                } else {
                    let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

                    let subscriber = sub_builder
                        .json()
                        .with_current_span(false)
                        .with_span_list(false)
                        .finish()
                        .with(telemetry);
                    if let Err(e) = set_global_default(subscriber) {
                        ::tracing::error!("cannot set global subscriber: {:?}", e);
                    }
                };
            }

            Ok(true)
        })?;

        let field_level_instrumentation_ratio =
            config.calculate_field_level_instrumentation_ratio()?;

        let plugin = Ok(Telemetry {
            custom_endpoints: builder.custom_endpoints(),
            _metrics_exporters: builder.exporters(),
            meter_provider: builder.meter_provider(),
            apollo_metrics_sender: builder.apollo_metrics_provider(),
            field_level_instrumentation_ratio,
            config,
        });

        // We're now safe for shutdown.
        // Start spaceport
        if let Some(spaceport) = spaceport {
            tokio::spawn(async move {
                ::tracing::debug!("serving spaceport");
                match spaceport.serve().await {
                    Ok(v) => {
                        ::tracing::debug!("spaceport terminated normally: {:?}", v);
                    }
                    Err(e) => match e.source() {
                        Some(source) => {
                            ::tracing::warn!("spaceport did not terminate normally: {}", source);
                        }
                        None => {
                            ::tracing::warn!("spaceport did not terminate normally: {}", e);
                        }
                    },
                }
                ::tracing::debug!("stopped serving spaceport");
            });
        }

        let _ = TELEMETRY_REFCOUNT.fetch_add(1, Ordering::Relaxed);
        plugin
    }

    fn create_propagator(config: &config::Conf) -> TextMapCompositePropagator {
        let propagation = config
            .clone()
            .tracing
            .and_then(|c| c.propagation)
            .unwrap_or_default();

        let tracing = config.clone().tracing.unwrap_or_default();

        let mut propagators: Vec<Box<dyn TextMapPropagator + Send + Sync + 'static>> = Vec::new();
        // TLDR the jaeger propagator MUST BE the first one because the version of opentelemetry_jaeger is buggy.
        // It overrides the current span context with an empty one if it doesn't find the corresponding headers.
        // Waiting for the >=0.16.1 release
        if propagation.jaeger.unwrap_or_default() || tracing.jaeger.is_some() {
            propagators.push(Box::new(opentelemetry_jaeger::Propagator::default()));
        }
        if propagation.baggage.unwrap_or_default() {
            propagators.push(Box::new(BaggagePropagator::default()));
        }
        if propagation.trace_context.unwrap_or_default() || tracing.otlp.is_some() {
            propagators.push(Box::new(TraceContextPropagator::default()));
        }
        if propagation.zipkin.unwrap_or_default() || tracing.zipkin.is_some() {
            propagators.push(Box::new(opentelemetry_zipkin::Propagator::default()));
        }
        if propagation.datadog.unwrap_or_default() || tracing.datadog.is_some() {
            propagators.push(Box::new(opentelemetry_datadog::DatadogPropagator::default()));
        }

        TextMapCompositePropagator::new(propagators)
    }

    fn create_tracer_provider(
        config: &config::Conf,
    ) -> Result<opentelemetry::sdk::trace::TracerProvider, BoxError> {
        let tracing_config = config.tracing.clone().unwrap_or_default();
        let trace_config = &tracing_config.trace_config.unwrap_or_default();
        let mut builder =
            opentelemetry::sdk::trace::TracerProvider::builder().with_config(trace_config.into());

        builder = setup_tracing(builder, &tracing_config.jaeger, trace_config)?;
        builder = setup_tracing(builder, &tracing_config.zipkin, trace_config)?;
        builder = setup_tracing(builder, &tracing_config.datadog, trace_config)?;
        builder = setup_tracing(builder, &tracing_config.otlp, trace_config)?;
        builder = setup_tracing(builder, &config.apollo, trace_config)?;

        let tracer_provider = builder.build();
        Ok(tracer_provider)
    }

    fn create_metrics_exporters(config: &config::Conf) -> Result<MetricsBuilder, BoxError> {
        let metrics_config = config.metrics.clone().unwrap_or_default();
        let metrics_common_config = &mut metrics_config.common.unwrap_or_default();
        // Set default service name for metrics
        if metrics_common_config
            .resources
            .get(opentelemetry_semantic_conventions::resource::SERVICE_NAME.as_str())
            .is_none()
        {
            metrics_common_config.resources.insert(
                String::from(opentelemetry_semantic_conventions::resource::SERVICE_NAME.as_str()),
                String::from(
                    metrics_common_config
                        .service_name
                        .as_deref()
                        .unwrap_or(DEFAULT_SERVICE_NAME),
                ),
            );
        }
        if let Some(service_namespace) = &metrics_common_config.service_namespace {
            metrics_common_config.resources.insert(
                String::from(
                    opentelemetry_semantic_conventions::resource::SERVICE_NAMESPACE.as_str(),
                ),
                service_namespace.clone(),
            );
        }

        let mut builder = MetricsBuilder::default();
        builder = setup_metrics_exporter(builder, &config.apollo, metrics_common_config)?;
        builder =
            setup_metrics_exporter(builder, &metrics_config.prometheus, metrics_common_config)?;
        builder = setup_metrics_exporter(builder, &metrics_config.otlp, metrics_common_config)?;
        Ok(builder)
    }

    fn supergraph_service_span(
        field_level_instrumentation_ratio: f64,
        config: apollo::Config,
    ) -> impl Fn(&SupergraphRequest) -> Span + Clone {
        move |request: &SupergraphRequest| {
            let http_request = &request.supergraph_request;
            let headers = http_request.headers();
            let query = http_request.body().query.clone().unwrap_or_default();
            let operation_name = http_request
                .body()
                .operation_name
                .clone()
                .unwrap_or_default();
            let client_name = headers
                .get(&config.client_name_header)
                .cloned()
                .unwrap_or_else(|| HeaderValue::from_static(""));
            let client_version = headers
                .get(&config.client_version_header)
                .cloned()
                .unwrap_or_else(|| HeaderValue::from_static(""));

            let span = info_span!(
                SUPERGRAPH_SPAN_NAME,
                graphql.document = query.as_str(),
                // TODO add graphql.operation.type
                graphql.operation.name = operation_name.as_str(),
                client.name = client_name.to_str().unwrap_or_default(),
                client.version = client_version.to_str().unwrap_or_default(),
                otel.kind = %SpanKind::Internal,
                apollo_private.field_level_instrumentation_ratio = field_level_instrumentation_ratio,
                apollo_private.operation_signature = field::Empty,
                apollo_private.graphql.variables = field::Empty,
                apollo_private.http.request_headers = field::Empty
            );

            if is_span_sampled(&request.context) {
                span.record(
                    "apollo_private.graphql.variables",
                    &Self::filter_variables_values(
                        &request.supergraph_request.body().variables,
                        &config.send_variable_values,
                    )
                    .as_str(),
                );
                span.record(
                    "apollo_private.http.request_headers",
                    &Self::filter_headers(
                        request.supergraph_request.headers(),
                        &config.send_headers,
                    )
                    .as_str(),
                );
            }

            span
        }
    }

    fn filter_headers(headers: &HeaderMap, forward_rules: &ForwardHeaders) -> String {
        let headers_map = headers
            .iter()
            .filter(|(name, _value)| {
                name != &header::AUTHORIZATION
                    && name != &header::COOKIE
                    && name != &header::SET_COOKIE
            })
            .map(|(name, value)| {
                if match &forward_rules {
                    ForwardHeaders::None => false,
                    ForwardHeaders::All => true,
                    ForwardHeaders::Only(only) => only.contains(name),
                    ForwardHeaders::Except(except) => !except.contains(name),
                } {
                    (
                        name.to_string(),
                        value.to_str().unwrap_or("<unknown>").to_string(),
                    )
                } else {
                    (name.to_string(), "".to_string())
                }
            })
            .fold(BTreeMap::new(), |mut acc, (name, value)| {
                acc.entry(name).or_insert_with(Vec::new).push(value);
                acc
            });

        match serde_json::to_string(&headers_map) {
            Ok(result) => result,
            Err(_err) => {
                ::tracing::warn!(
                    "could not serialize header, trace will not have header information"
                );
                Default::default()
            }
        }
    }

    fn filter_variables_values(
        variables: &Map<ByteString, Value>,
        forward_rules: &ForwardValues,
    ) -> String {
        #[allow(clippy::mutable_key_type)] // False positive lint
        let variables = variables
            .iter()
            .map(|(name, value)| {
                if match &forward_rules {
                    ForwardValues::None => false,
                    ForwardValues::All => true,
                    ForwardValues::Only(only) => only.contains(&name.as_str().to_string()),
                    ForwardValues::Except(except) => !except.contains(&name.as_str().to_string()),
                } {
                    (
                        name,
                        serde_json::to_string(value).unwrap_or_else(|_| "<unknown>".to_string()),
                    )
                } else {
                    (name, "".to_string())
                }
            })
            .fold(BTreeMap::new(), |mut acc, (name, value)| {
                acc.entry(name).or_insert_with(Vec::new).push(value);
                acc
            });

        match serde_json::to_string(&variables) {
            Ok(result) => result,
            Err(_err) => {
                ::tracing::warn!(
                    "could not serialize variables, trace will not have variables information"
                );
                Default::default()
            }
        }
    }

    async fn update_otel_metrics(
        config: Arc<Conf>,
        context: Context,
        metrics: BasicMetrics,
        result: Result<SupergraphResponse, BoxError>,
        request_duration: Duration,
    ) -> Result<SupergraphResponse, BoxError> {
        let mut metric_attrs = context
            .get::<_, HashMap<String, String>>(ATTRIBUTES)
            .ok()
            .flatten()
            .map(|attrs| {
                attrs
                    .into_iter()
                    .map(|(attr_name, attr_value)| KeyValue::new(attr_name, attr_value))
                    .collect::<Vec<KeyValue>>()
            })
            .unwrap_or_default();
        let res = match result {
            Ok(response) => {
                metric_attrs.push(KeyValue::new(
                    "status",
                    response.response.status().as_u16().to_string(),
                ));

                // Wait for the first response of the stream
                let (parts, stream) = response.response.into_parts();
                let (first_response, rest) = stream.into_future().await;

                if let Some(MetricsCommon {
                    attributes:
                        Some(MetricsAttributesConf {
                            router: Some(forward_attributes),
                            ..
                        }),
                    ..
                }) = &config.metrics.as_ref().and_then(|m| m.common.as_ref())
                {
                    let attributes = forward_attributes.get_attributes_from_router_response(
                        &parts,
                        &context,
                        &first_response,
                    );

                    metric_attrs.extend(attributes.into_iter().map(|(k, v)| KeyValue::new(k, v)));
                    metrics.http_requests_total.add(1, &metric_attrs);
                } else {
                    metrics.http_requests_total.add(1, &metric_attrs);
                }

                let response = http::Response::from_parts(
                    parts,
                    once(ready(first_response.unwrap_or_default()))
                        .chain(rest)
                        .boxed(),
                );

                Ok(SupergraphResponse { context, response })
            }
            Err(err) => {
                metrics.http_requests_error_total.add(1, &metric_attrs);

                Err(err)
            }
        };
        metrics
            .http_requests_duration
            .record(request_duration.as_secs_f64(), &metric_attrs);

        res
    }

    fn populate_context(config: Arc<Conf>, req: &SupergraphRequest) {
        let apollo_config = config.apollo.clone().unwrap_or_default();
        let context = &req.context;
        let http_request = &req.supergraph_request;
        let headers = http_request.headers();
        let client_name_header = &apollo_config.client_name_header;
        let client_version_header = &apollo_config.client_version_header;
        let _ = context.insert(
            CLIENT_NAME,
            headers
                .get(client_name_header)
                .cloned()
                .unwrap_or_else(|| HeaderValue::from_static(""))
                .to_str()
                .unwrap_or_default()
                .to_string(),
        );
        let _ = context.insert(
            CLIENT_VERSION,
            headers
                .get(client_version_header)
                .cloned()
                .unwrap_or_else(|| HeaderValue::from_static(""))
                .to_str()
                .unwrap_or_default()
                .to_string(),
        );
        if let Some(metrics_conf) = &config.metrics {
            // List of custom attributes for metrics
            let mut attributes: HashMap<String, String> = HashMap::new();
            if let Some(operation_name) = &req.supergraph_request.body().operation_name {
                attributes.insert("operation_name".to_string(), operation_name.clone());
            }

            if let Some(router_attributes_conf) = metrics_conf
                .common
                .as_ref()
                .and_then(|c| c.attributes.as_ref())
                .and_then(|a| a.router.as_ref())
            {
                attributes.extend(
                    router_attributes_conf
                        .get_attributes_from_request(headers, req.supergraph_request.body()),
                );
                attributes.extend(router_attributes_conf.get_attributes_from_context(context));
            }

            let _ = context.insert(ATTRIBUTES, attributes);
        }
    }

    fn apollo_handler(&self) -> ApolloFtv1Handler {
        let mut rng = rand::thread_rng();

        if rng.gen_ratio((self.field_level_instrumentation_ratio * 100.0) as u32, 100) {
            ApolloFtv1Handler::Enabled
        } else {
            ApolloFtv1Handler::Disabled
        }
    }

    fn create_subgraph_metrics_conf(&self, name: &str) -> Arc<Option<AttributesForwardConf>> {
        Arc::new(
            self.config
                .metrics
                .as_ref()
                .and_then(|m| m.common.as_ref())
                .and_then(|c| c.attributes.as_ref())
                .and_then(|c| c.subgraph.as_ref())
                .map(|subgraph_cfg| {
                    macro_rules! extend_config {
                        ($forward_kind: ident) => {{
                            let mut cfg = subgraph_cfg
                                .all
                                .as_ref()
                                .and_then(|a| a.$forward_kind.clone())
                                .unwrap_or_default();
                            if let Some(subgraphs) = &subgraph_cfg.subgraphs {
                                cfg.extend(
                                    subgraphs
                                        .get(&name.to_owned())
                                        .and_then(|s| s.$forward_kind.clone())
                                        .unwrap_or_default(),
                                );
                            }

                            cfg
                        }};
                    }
                    macro_rules! merge_config {
                        ($forward_kind: ident) => {{
                            let mut cfg = subgraph_cfg
                                .all
                                .as_ref()
                                .and_then(|a| a.$forward_kind.clone())
                                .unwrap_or_default();
                            if let Some(subgraphs) = &subgraph_cfg.subgraphs {
                                cfg.merge(
                                    subgraphs
                                        .get(&name.to_owned())
                                        .and_then(|s| s.$forward_kind.clone())
                                        .unwrap_or_default(),
                                );
                            }

                            cfg
                        }};
                    }
                    let insert = extend_config!(insert);
                    let context = extend_config!(context);
                    let request = merge_config!(request);
                    let response = merge_config!(response);
                    let errors = merge_config!(errors);

                    AttributesForwardConf {
                        insert: (!insert.is_empty()).then(|| insert),
                        request: (request.header.is_some() || request.body.is_some())
                            .then(|| request),
                        response: (response.header.is_some() || response.body.is_some())
                            .then(|| response),
                        errors: (errors.extensions.is_some() || errors.include_messages)
                            .then(|| errors),
                        context: (!context.is_empty()).then(|| context),
                    }
                }),
        )
    }

    fn store_subgraph_request_attributes(
        attribute_forward_config: Arc<Option<AttributesForwardConf>>,
        sub_request: &Request,
    ) {
        let mut attributes = HashMap::new();
        if let Some(subgraph_attributes_conf) = &*attribute_forward_config {
            attributes.extend(subgraph_attributes_conf.get_attributes_from_request(
                sub_request.subgraph_request.headers(),
                sub_request.subgraph_request.body(),
            ));
            attributes
                .extend(subgraph_attributes_conf.get_attributes_from_context(&sub_request.context));
        }
        sub_request
            .context
            .insert(SUBGRAPH_ATTRIBUTES, attributes)
            .unwrap();
    }

    fn store_subgraph_response_attributes(
        context: &Context,
        metrics: BasicMetrics,
        subgraph_attribute: KeyValue,
        attribute_forward_config: Arc<Option<AttributesForwardConf>>,
        now: Instant,
        result: &Result<Response, BoxError>,
    ) {
        let mut metric_attrs = context
            .get::<_, HashMap<String, String>>(SUBGRAPH_ATTRIBUTES)
            .ok()
            .flatten()
            .map(|attrs| {
                attrs
                    .into_iter()
                    .map(|(attr_name, attr_value)| KeyValue::new(attr_name, attr_value))
                    .collect::<Vec<KeyValue>>()
            })
            .unwrap_or_default();
        metric_attrs.push(subgraph_attribute);
        // Fill attributes from context
        if let Some(subgraph_attributes_conf) = &*attribute_forward_config {
            metric_attrs.extend(
                subgraph_attributes_conf
                    .get_attributes_from_context(context)
                    .into_iter()
                    .map(|(k, v)| KeyValue::new(k, v)),
            );
        }

        match &result {
            Ok(response) => {
                metric_attrs.push(KeyValue::new(
                    "status",
                    response.response.status().as_u16().to_string(),
                ));

                // Fill attributes from response
                if let Some(subgraph_attributes_conf) = &*attribute_forward_config {
                    metric_attrs.extend(
                        subgraph_attributes_conf
                            .get_attributes_from_response(
                                response.response.headers(),
                                response.response.body(),
                            )
                            .into_iter()
                            .map(|(k, v)| KeyValue::new(k, v)),
                    );
                }

                metrics.http_requests_total.add(1, &metric_attrs);
            }
            Err(err) => {
                // Fill attributes from error
                if let Some(subgraph_attributes_conf) = &*attribute_forward_config {
                    metric_attrs.extend(
                        subgraph_attributes_conf
                            .get_attributes_from_error(err)
                            .into_iter()
                            .map(|(k, v)| KeyValue::new(k, v)),
                    );
                }

                metrics.http_requests_error_total.add(1, &metric_attrs);
            }
        }
        metrics
            .http_requests_duration
            .record(now.elapsed().as_secs_f64(), &metric_attrs);
    }

    #[allow(clippy::too_many_arguments)]
    fn update_metrics_on_last_response(
        ctx: &Context,
        config: Arc<Conf>,
        metrics: BasicMetrics,
        sender: Sender,
        start: Instant,
        result: Result<supergraph::Response, BoxError>,
    ) -> Result<supergraph::Response, BoxError> {
        match result {
            Err(e) => {
                if !matches!(sender, Sender::Noop) {
                    Self::update_apollo_metrics(ctx, sender, true, start.elapsed());
                }
                let mut metric_attrs = Vec::new();
                // Fill attributes from error
                if let Some(subgraph_attributes_conf) = config
                    .metrics
                    .as_ref()
                    .and_then(|m| m.common.as_ref())
                    .and_then(|c| c.attributes.as_ref())
                    .and_then(|c| c.router.as_ref())
                {
                    metric_attrs.extend(
                        subgraph_attributes_conf
                            .get_attributes_from_error(&e)
                            .into_iter()
                            .map(|(k, v)| KeyValue::new(k, v)),
                    );
                }

                metrics.http_requests_error_total.add(1, &metric_attrs);

                Err(e)
            }
            Ok(router_response) => {
                let mut has_errors = !router_response.response.status().is_success();
                Ok(router_response.map(move |response_stream| {
                    let sender = sender.clone();
                    let ctx = ctx.clone();

                    response_stream
                        .map(move |response| {
                            if !response.errors.is_empty() {
                                has_errors = true;
                            }

                            if !response.has_next.unwrap_or(false)
                                && !matches!(sender, Sender::Noop)
                            {
                                Self::update_apollo_metrics(
                                    &ctx,
                                    sender.clone(),
                                    has_errors,
                                    start.elapsed(),
                                );
                            }
                            response
                        })
                        .boxed()
                }))
            }
        }
    }

    fn update_apollo_metrics(
        context: &Context,
        sender: Sender,
        has_errors: bool,
        duration: Duration,
    ) {
        if is_span_sampled(context) {
            ::tracing::trace!("span is sampled then skip the apollo metrics");
            return;
        }
        let metrics = if let Some(usage_reporting) = context
            .get::<_, UsageReporting>(USAGE_REPORTING)
            .unwrap_or_default()
        {
            let operation_count = operation_count(&usage_reporting.stats_report_key);
            let persisted_query_hit = context
                .get::<_, bool>("persisted_query_hit")
                .unwrap_or_default();

            if context
                .get(STUDIO_EXCLUDE)
                .map_or(false, |x| x.unwrap_or_default())
            {
                // The request was excluded don't report the details, but do report the operation count
                SingleStatsReport {
                    operation_count,
                    ..Default::default()
                }
            } else {
                SingleStatsReport {
                    request_id: uuid::Uuid::from_bytes(
                        Span::current()
                            .context()
                            .span()
                            .span_context()
                            .trace_id()
                            .to_bytes(),
                    ),
                    operation_count,
                    stats: HashMap::from([(
                        usage_reporting.stats_report_key.to_string(),
                        SingleStats {
                            stats_with_context: SingleContextualizedStats {
                                context: StatsContext {
                                    client_name: context
                                        .get(CLIENT_NAME)
                                        .unwrap_or_default()
                                        .unwrap_or_default(),
                                    client_version: context
                                        .get(CLIENT_VERSION)
                                        .unwrap_or_default()
                                        .unwrap_or_default(),
                                },
                                query_latency_stats: SingleQueryLatencyStats {
                                    latency: duration,
                                    has_errors,
                                    persisted_query_hit,
                                    ..Default::default()
                                },
                                ..Default::default()
                            },
                            referenced_fields_by_type: usage_reporting
                                .referenced_fields_by_type
                                .into_iter()
                                .map(|(k, v)| (k, convert(v)))
                                .collect(),
                        },
                    )]),
                }
            }
        } else {
            // Usage reporting was missing, so it counts as one operation.
            SingleStatsReport {
                operation_count: 1,
                ..Default::default()
            }
        };
        sender.send(SingleReport::Stats(metrics));
    }
}

// Planner errors return stats report key that start with `## `
// while successful planning stats report key start with `# `
fn operation_count(stats_report_key: &str) -> u64 {
    if stats_report_key.starts_with("## ") {
        0
    } else {
        1
    }
}

fn convert(
    referenced_fields: router_bridge::planner::ReferencedFieldsForType,
) -> crate::spaceport::ReferencedFieldsForType {
    crate::spaceport::ReferencedFieldsForType {
        field_names: referenced_fields.field_names,
        is_interface: referenced_fields.is_interface,
    }
}

fn handle_error<T: Into<opentelemetry::global::Error>>(err: T) {
    match err.into() {
        opentelemetry::global::Error::Trace(err) => {
            ::tracing::error!("OpenTelemetry trace error occurred: {}", err)
        }
        opentelemetry::global::Error::Other(err_msg) => {
            ::tracing::error!("OpenTelemetry error occurred: {}", err_msg)
        }
        other => {
            ::tracing::error!("OpenTelemetry error occurred: {:?}", other)
        }
    }
}

#[inline]
pub(crate) fn is_span_sampled(context: &Context) -> bool {
    Span::current().context().span().span_context().is_sampled()
        && !context
            .get(FTV1_DO_NOT_SAMPLE)
            .unwrap_or_default()
            .unwrap_or(false)
}

register_plugin!("apollo", "telemetry", Telemetry);

/// This enum is a partial cleanup of the telemetry plugin logic.
///
#[derive(Copy, Clone)]
enum ApolloFtv1Handler {
    Enabled,
    Disabled,
}

impl ApolloFtv1Handler {
    fn request_ftv1(&self, mut req: SubgraphRequest) -> SubgraphRequest {
        if let ApolloFtv1Handler::Enabled = self {
            if is_span_sampled(&req.context) {
                req.subgraph_request.headers_mut().insert(
                    "apollo-federation-include-trace",
                    HeaderValue::from_static("ftv1"),
                );
            }
        }
        req
    }

    fn store_ftv1(&self, resp: SubgraphResponse) -> SubgraphResponse {
        // Stash the FTV1 data
        if let ApolloFtv1Handler::Enabled = self {
            if let Some(serde_json_bytes::Value::String(ftv1)) =
                resp.response.body().extensions.get("ftv1")
            {
                // Record the ftv1 trace for processing later
                Span::current().record("apollo_private.ftv1", &ftv1.as_str());
            }
        }
        resp
    }
}

//
// Please ensure that any tests added to the tests module use the tokio multi-threaded test executor.
//
#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use http::StatusCode;
    use serde_json::Value;
    use serde_json_bytes::json;
    use serde_json_bytes::ByteString;
    use tower::util::BoxService;
    use tower::Service;
    use tower::ServiceExt;

    use crate::error::FetchError;
    use crate::graphql::Error;
    use crate::graphql::Request;
    use crate::http_ext;
    use crate::json_ext::Object;
    use crate::plugin::test::MockSubgraphService;
    use crate::plugin::test::MockSupergraphService;
    use crate::plugin::DynPlugin;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::SupergraphRequest;
    use crate::SupergraphResponse;

    #[tokio::test(flavor = "multi_thread")]
    async fn plugin_registered() {
        crate::plugin::plugins()
            .get("apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(
                &serde_json::json!({"apollo": {"schema_id":"abc"}, "tracing": {}}),
                Default::default(),
            )
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn attribute_serialization() {
        crate::plugin::plugins()
            .get("apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(
                &serde_json::json!({
                    "apollo": {"schema_id":"abc"},
                    "tracing": {
                        "trace_config": {
                            "service_name": "router",
                            "attributes": {
                                "str": "a",
                                "int": 1,
                                "float": 1.0,
                                "bool": true,
                                "str_arr": ["a", "b"],
                                "int_arr": [1, 2],
                                "float_arr": [1.0, 2.0],
                                "bool_arr": [true, false]
                            }
                        }
                    },
                    "metrics": {
                        "common": {
                            "attributes": {
                                "router": {
                                    "static": [
                                        {
                                            "name": "myname",
                                            "value": "label_value"
                                        }
                                    ],
                                    "request": {
                                        "header": [{
                                            "named": "test",
                                            "default": "default_value",
                                            "rename": "renamed_value"
                                        }],
                                        "body": [{
                                            "path": ".data.test",
                                            "name": "my_new_name",
                                            "default": "default_value"
                                        }]
                                    },
                                    "response": {
                                        "header": [{
                                            "named": "test",
                                            "default": "default_value",
                                            "rename": "renamed_value",
                                        }, {
                                            "named": "test",
                                            "default": "default_value",
                                            "rename": "renamed_value",
                                        }],
                                        "body": [{
                                            "path": ".data.test",
                                            "name": "my_new_name",
                                            "default": "default_value"
                                        }]
                                    }
                                },
                                "subgraph": {
                                    "all": {
                                        "static": [
                                            {
                                                "name": "myname",
                                                "value": "label_value"
                                            }
                                        ],
                                        "request": {
                                            "header": [{
                                                "named": "test",
                                                "default": "default_value",
                                                "rename": "renamed_value",
                                            }],
                                            "body": [{
                                                "path": ".data.test",
                                                "name": "my_new_name",
                                                "default": "default_value"
                                            }]
                                        },
                                        "response": {
                                            "header": [{
                                                "named": "test",
                                                "default": "default_value",
                                                "rename": "renamed_value",
                                            }, {
                                                "named": "test",
                                                "default": "default_value",
                                                "rename": "renamed_value",
                                            }],
                                            "body": [{
                                                "path": ".data.test",
                                                "name": "my_new_name",
                                                "default": "default_value"
                                            }]
                                        }
                                    },
                                    "subgraphs": {
                                        "subgraph_name_test": {
                                             "static": [
                                                {
                                                    "name": "myname",
                                                    "value": "label_value"
                                                }
                                            ],
                                            "request": {
                                                "header": [{
                                                    "named": "test",
                                                    "default": "default_value",
                                                    "rename": "renamed_value",
                                                }],
                                                "body": [{
                                                    "path": ".data.test",
                                                    "name": "my_new_name",
                                                    "default": "default_value"
                                                }]
                                            },
                                            "response": {
                                                "header": [{
                                                    "named": "test",
                                                    "default": "default_value",
                                                    "rename": "renamed_value",
                                                }, {
                                                    "named": "test",
                                                    "default": "default_value",
                                                    "rename": "renamed_value",
                                                }],
                                                "body": [{
                                                    "path": ".data.test",
                                                    "name": "my_new_name",
                                                    "default": "default_value"
                                                }]
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }),
                Default::default(),
            )
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_prometheus_metrics() {
        let mut mock_service = MockSupergraphService::new();
        mock_service
            .expect_call()
            .times(1)
            .returning(move |req: SupergraphRequest| {
                Ok(SupergraphResponse::fake_builder()
                    .context(req.context)
                    .header("x-custom", "coming_from_header")
                    .data(json!({"data": {"my_value": 2usize}}))
                    .build()
                    .unwrap())
            });

        let mut mock_subgraph_service = MockSubgraphService::new();
        mock_subgraph_service
            .expect_call()
            .times(1)
            .returning(move |req: SubgraphRequest| {
                let mut extension = Object::new();
                extension.insert(
                    serde_json_bytes::ByteString::from("status"),
                    serde_json_bytes::Value::String(ByteString::from("INTERNAL_SERVER_ERROR")),
                );
                let _ = req
                    .context
                    .insert("my_key", "my_custom_attribute_from_context".to_string())
                    .unwrap();
                Ok(SubgraphResponse::fake_builder()
                    .context(req.context)
                    .error(
                        Error::builder()
                            .message(String::from("an error occured"))
                            .extensions(extension)
                            .build(),
                    )
                    .build())
            });

        let mut mock_subgraph_service_in_error = MockSubgraphService::new();
        mock_subgraph_service_in_error
            .expect_call()
            .times(1)
            .returning(move |_req: SubgraphRequest| {
                Err(Box::new(FetchError::SubrequestHttpError {
                    service: String::from("my_subgraph_name_error"),
                    reason: String::from("cannot contact the subgraph"),
                }))
            });

        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .get("apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(
                &Value::from_str(
                    r#"{
                "apollo": {
                    "client_name_header": "name_header",
                    "client_version_header": "version_header",
                    "schema_id": "schema_sha"
                },
                "metrics": {
                    "common": {
                        "service_name": "apollo-router",
                        "attributes": {
                            "router": {
                                "static": [
                                    {
                                        "name": "myname",
                                        "value": "label_value"
                                    }
                                ],
                                "request": {
                                    "header": [
                                        {
                                            "named": "test",
                                            "default": "default_value",
                                            "rename": "renamed_value"
                                        },
                                        {
                                            "named": "another_test",
                                            "default": "my_default_value"
                                        }
                                    ]
                                },
                                "response": {
                                    "header": [{
                                        "named": "x-custom"
                                    }],
                                    "body": [{
                                        "path": ".data.data.my_value",
                                        "name": "my_value"
                                    }]
                                }
                            },
                            "subgraph": {
                                "all": {
                                    "errors": {
                                        "include_messages": true,
                                        "extensions": [{
                                            "name": "subgraph_error_extended_type",
                                            "path": ".type"
                                        }, {
                                            "name": "message",
                                            "path": ".reason"
                                        }]
                                    }
                                },
                                "subgraphs": {
                                    "my_subgraph_name": {
                                        "request": {
                                            "body": [{
                                                "path": ".query",
                                                "name": "query_from_request"
                                            }, {
                                                "path": ".data",
                                                "name": "unknown_data",
                                                "default": "default_value"
                                            }, {
                                                "path": ".data2",
                                                "name": "unknown_data_bis"
                                            }]
                                        },
                                        "response": {
                                            "body": [{
                                                "path": ".errors[0].extensions.status",
                                                "name": "error"
                                            }]
                                        },
                                        "context": [
                                            {
                                                "named": "my_key"
                                            }
                                        ]
                                    }
                                }
                            }
                        }
                    },
                    "prometheus": {
                        "enabled": true
                    }
                }
            }"#,
                )
                .unwrap(),
                Default::default(),
            )
            .await
            .unwrap();
        let mut supergraph_service = dyn_plugin.supergraph_service(BoxService::new(mock_service));
        let router_req = SupergraphRequest::fake_builder().header("test", "my_value_set");

        let _router_response = supergraph_service
            .ready()
            .await
            .unwrap()
            .call(router_req.build().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        let mut subgraph_service =
            dyn_plugin.subgraph_service("my_subgraph_name", BoxService::new(mock_subgraph_service));
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
            .build();
        let _subgraph_response = subgraph_service
            .ready()
            .await
            .unwrap()
            .call(subgraph_req)
            .await
            .unwrap();
        // Another subgraph
        let mut subgraph_service = dyn_plugin.subgraph_service(
            "my_subgraph_name_error",
            BoxService::new(mock_subgraph_service_in_error),
        );
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
            .build();
        let _subgraph_response = subgraph_service
            .ready()
            .await
            .unwrap()
            .call(subgraph_req)
            .await
            .expect_err("Must be in error");

        let http_req_prom = http::Request::get("http://localhost:9090/WRONG/URL/metrics")
            .body(Default::default())
            .unwrap();
        let mut web_endpoint = dyn_plugin
            .web_endpoints()
            .into_iter()
            .next()
            .unwrap()
            .1
            .into_iter()
            .next()
            .unwrap()
            .into_router();
        let resp = web_endpoint
            .ready()
            .await
            .unwrap()
            .call(http_req_prom)
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let http_req_prom = http::Request::get("http://localhost:9090/metrics")
            .body(Default::default())
            .unwrap();
        let mut resp = web_endpoint.oneshot(http_req_prom).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(resp.body_mut()).await.unwrap();
        let prom_metrics = String::from_utf8_lossy(&body);
        assert!(prom_metrics.contains(r#"http_requests_error_total{message="cannot contact the subgraph",service_name="apollo-router",subgraph="my_subgraph_name_error",subgraph_error_extended_type="SubrequestHttpError"} 1"#));
        assert!(prom_metrics.contains(r#"http_requests_total{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header"} 1"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_count{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="0.001"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="0.005"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="0.015"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="0.05"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="0.3"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="0.4"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="0.5"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="1"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="5"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="10"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header",le="+Inf"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_count{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_sum{another_test="my_default_value",my_value="2",myname="label_value",renamed_value="my_value_set",service_name="apollo-router",status="200",x_custom="coming_from_header"}"#));
        assert!(prom_metrics.contains(r#"http_request_duration_seconds_bucket{error="INTERNAL_SERVER_ERROR",my_key="my_custom_attribute_from_context",query_from_request="query { test }",service_name="apollo-router",status="200",subgraph="my_subgraph_name",unknown_data="default_value",le="1"}"#));
    }
}
