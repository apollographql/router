//! Configuration for datadog tracing.

mod agent_sampling;
mod agent_span_processor;

use std::fmt::Debug;
use std::fmt::Formatter;
use std::time::Duration;

pub(crate) use agent_sampling::AgentSampling;
pub(crate) use agent_span_processor::BatchSpanProcessor;
use ahash::HashMap;
use ahash::HashMapExt;
use futures::future::BoxFuture;
use http::Uri;
use opentelemetry::sdk;
use opentelemetry::sdk::trace::Builder;
use opentelemetry::Value;
use opentelemetry_api::trace::SpanContext;
use opentelemetry_api::trace::SpanKind;
use opentelemetry_api::Key;
use opentelemetry_api::KeyValue;
use opentelemetry_sdk::export::trace::ExportResult;
use opentelemetry_sdk::export::trace::SpanData;
use opentelemetry_sdk::export::trace::SpanExporter;
use opentelemetry_semantic_conventions::resource::SERVICE_NAME;
use opentelemetry_semantic_conventions::resource::SERVICE_VERSION;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::telemetry::config::GenericWith;
use crate::plugins::telemetry::config::TracingCommon;
use crate::plugins::telemetry::config_new::spans::Spans;
use crate::plugins::telemetry::consts::BUILT_IN_SPAN_NAMES;
use crate::plugins::telemetry::consts::HTTP_REQUEST_SPAN_NAME;
use crate::plugins::telemetry::consts::OTEL_ORIGINAL_NAME;
use crate::plugins::telemetry::consts::QUERY_PLANNING_SPAN_NAME;
use crate::plugins::telemetry::consts::REQUEST_SPAN_NAME;
use crate::plugins::telemetry::consts::ROUTER_SPAN_NAME;
use crate::plugins::telemetry::consts::SUBGRAPH_REQUEST_SPAN_NAME;
use crate::plugins::telemetry::consts::SUBGRAPH_SPAN_NAME;
use crate::plugins::telemetry::consts::SUPERGRAPH_SPAN_NAME;
use crate::plugins::telemetry::endpoint::UriEndpoint;
use crate::plugins::telemetry::tracing::datadog_exporter;
use crate::plugins::telemetry::tracing::datadog_exporter::DatadogTraceState;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::tracing::SpanProcessorExt;
use crate::plugins::telemetry::tracing::TracingConfigurator;

fn default_resource_mappings() -> HashMap<String, String> {
    let mut map = HashMap::with_capacity(7);
    map.insert(REQUEST_SPAN_NAME, "http.route");
    map.insert(ROUTER_SPAN_NAME, "http.route");
    map.insert(SUPERGRAPH_SPAN_NAME, "graphql.operation.name");
    map.insert(QUERY_PLANNING_SPAN_NAME, "graphql.operation.name");
    map.insert(SUBGRAPH_SPAN_NAME, "subgraph.name");
    map.insert(SUBGRAPH_REQUEST_SPAN_NAME, "graphql.operation.name");
    map.insert(HTTP_REQUEST_SPAN_NAME, "http.route");
    map.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

const ENV_KEY: Key = Key::from_static_str("env");
const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8126";

#[derive(Debug, Clone, Deserialize, JsonSchema, serde_derive_default::Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// Enable datadog
    enabled: bool,

    /// The endpoint to send to
    #[serde(default)]
    endpoint: UriEndpoint,

    /// batch processor configuration
    #[serde(default)]
    batch_processor: BatchProcessorConfig,

    /// Enable datadog span mapping for span name and resource name.
    #[serde(default = "default_true")]
    enable_span_mapping: bool,

    /// Fixes the span names, this means that the APM view will show the original span names in the operation dropdown.
    #[serde(default = "default_true")]
    fixed_span_names: bool,

    /// Custom mapping to be used as the resource field in spans, defaults to:
    /// router -> http.route
    /// supergraph -> graphql.operation.name
    /// query_planning -> graphql.operation.name
    /// subgraph -> subgraph.name
    /// subgraph_request -> subgraph.name
    /// http_request -> http.route
    #[serde(default)]
    resource_mapping: HashMap<String, String>,

    /// Which spans will be eligible for span stats to be collected for viewing in the APM view.
    /// Defaults to true for `request`, `router`, `query_parsing`, `supergraph`, `execution`, `query_planning`, `subgraph`, `subgraph_request` and `http_request`.
    #[serde(default = "default_span_metrics")]
    span_metrics: HashMap<String, bool>,
}

fn default_span_metrics() -> HashMap<String, bool> {
    let mut map = HashMap::with_capacity(BUILT_IN_SPAN_NAMES.len());
    for name in BUILT_IN_SPAN_NAMES {
        map.insert(name.to_string(), true);
    }
    map
}

fn default_true() -> bool {
    true
}

impl TracingConfigurator for Config {
    fn enabled(&self) -> bool {
        self.enabled
    }

    fn apply(
        &self,
        builder: Builder,
        trace: &TracingCommon,
        _spans_config: &Spans,
    ) -> Result<Builder, BoxError> {
        tracing::info!("Configuring Datadog tracing: {}", self.batch_processor);

        let common: sdk::trace::Config = trace.into();

        // Precompute representation otel Keys for the mappings so that we don't do heap allocation for each span
        let resource_mappings = self.enable_span_mapping.then(|| {
            let mut resource_mappings = default_resource_mappings();
            resource_mappings.extend(self.resource_mapping.clone());
            resource_mappings
                .iter()
                .map(|(k, v)| (k.clone(), opentelemetry::Key::from(v.clone())))
                .collect::<HashMap<String, Key>>()
        });

        let fixed_span_names = self.fixed_span_names;

        let exporter = datadog_exporter::new_pipeline()
            .with(
                &self.endpoint.to_uri(&Uri::from_static(DEFAULT_ENDPOINT)),
                |builder, e| builder.with_agent_endpoint(e.to_string().trim_end_matches('/')),
            )
            .with(&resource_mappings, |builder, resource_mappings| {
                let resource_mappings = resource_mappings.clone();
                builder.with_resource_mapping(move |span, _model_config| {
                    let span_name = if let Some(original) = span
                        .attributes
                        .get(&Key::from_static_str(OTEL_ORIGINAL_NAME))
                    {
                        original.as_str()
                    } else {
                        span.name.clone()
                    };
                    if let Some(mapping) = resource_mappings.get(span_name.as_ref()) {
                        if let Some(Value::String(value)) = span.attributes.get(mapping) {
                            return value.as_str();
                        }
                    }
                    return span.name.as_ref();
                })
            })
            .with_name_mapping(move |span, _model_config| {
                if fixed_span_names {
                    if let Some(original) = span
                        .attributes
                        .get(&Key::from_static_str(OTEL_ORIGINAL_NAME))
                    {
                        // Datadog expects static span names, not the ones in the otel spec.
                        // Remap the span name to the original name if it was remapped.
                        for name in BUILT_IN_SPAN_NAMES {
                            if name == original.as_str() {
                                return name;
                            }
                        }
                    }
                }
                &span.name
            })
            .with(
                &common.resource.get(SERVICE_NAME),
                |builder, service_name| {
                    // Datadog exporter incorrectly ignores the service name in the resource
                    // Set it explicitly here
                    builder.with_service_name(service_name.as_str())
                },
            )
            .with(&common.resource.get(ENV_KEY), |builder, env| {
                builder.with_env(env.as_str())
            })
            .with_version(
                common
                    .resource
                    .get(SERVICE_VERSION)
                    .expect("cargo version is set as a resource default;qed")
                    .to_string(),
            )
            .with_http_client(
                reqwest::Client::builder()
                    // https://github.com/open-telemetry/opentelemetry-rust-contrib/issues/7
                    // Set the idle timeout to something low to prevent termination of connections.
                    .pool_idle_timeout(Duration::from_millis(1))
                    .build()?,
            )
            .with_trace_config(common)
            .build_exporter()?;

        // Use the default span metrics and override with the ones from the config
        let mut span_metrics = default_span_metrics();
        span_metrics.extend(self.span_metrics.clone());

        let batch_processor = opentelemetry::sdk::trace::BatchSpanProcessor::builder(
            ExporterWrapper {
                delegate: exporter,
                span_metrics,
            },
            opentelemetry::runtime::Tokio,
        )
        .with_batch_config(self.batch_processor.clone().into())
        .build()
        .filtered();

        Ok(
            if trace.preview_datadog_agent_sampling.unwrap_or_default() {
                builder.with_span_processor(batch_processor.datadog_agent())
            } else {
                builder.with_span_processor(batch_processor)
            },
        )
    }
}

struct ExporterWrapper {
    delegate: datadog_exporter::DatadogExporter,
    span_metrics: HashMap<String, bool>,
}

impl Debug for ExporterWrapper {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.delegate.fmt(f)
    }
}

impl SpanExporter for ExporterWrapper {
    fn export(&mut self, mut batch: Vec<SpanData>) -> BoxFuture<'static, ExportResult> {
        // Here we do some special processing of the spans before passing them to the delegate
        // In particular we default the span.kind to the span kind, and also override the trace measure status if we need to.
        for span in &mut batch {
            // If the span metrics are enabled for this span, set the trace state to measuring.
            // We do all this dancing to avoid allocating.
            let original_span_name = span
                .attributes
                .get(&Key::from_static_str(OTEL_ORIGINAL_NAME))
                .map(|v| v.as_str());
            let final_span_name = if let Some(span_name) = &original_span_name {
                span_name.as_ref()
            } else {
                span.name.as_ref()
            };

            // Unfortunately trace state is immutable, so we have to create a new one
            if let Some(setting) = self.span_metrics.get(final_span_name) {
                if *setting != span.span_context.trace_state().measuring_enabled() {
                    let new_trace_state = span.span_context.trace_state().with_measuring(*setting);
                    span.span_context = SpanContext::new(
                        span.span_context.trace_id(),
                        span.span_context.span_id(),
                        span.span_context.trace_flags(),
                        span.span_context.is_remote(),
                        new_trace_state,
                    )
                }
            }

            // Set the span kind https://github.com/DataDog/dd-trace-go/blob/main/ddtrace/ext/span_kind.go
            let span_kind = match &span.span_kind {
                SpanKind::Client => "client",
                SpanKind::Server => "server",
                SpanKind::Producer => "producer",
                SpanKind::Consumer => "consumer",
                SpanKind::Internal => "internal",
            };
            span.attributes
                .insert(KeyValue::new("span.kind", span_kind));

            // Note we do NOT set span.type as it isn't a good fit for otel.
        }
        self.delegate.export(batch)
    }
    fn shutdown(&mut self) {
        self.delegate.shutdown()
    }
    fn force_flush(&mut self) -> BoxFuture<'static, ExportResult> {
        self.delegate.force_flush()
    }
}
