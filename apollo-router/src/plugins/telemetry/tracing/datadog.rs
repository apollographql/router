//! Configuration for datadog tracing.

use std::collections::HashMap;

use http::Uri;
use opentelemetry::sdk;
use opentelemetry::sdk::trace::BatchSpanProcessor;
use opentelemetry::sdk::trace::Builder;
use opentelemetry::Value;
use opentelemetry_api::Key;
use opentelemetry_semantic_conventions::resource::SERVICE_NAME;
use opentelemetry_semantic_conventions::resource::SERVICE_VERSION;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::plugins::telemetry::config::GenericWith;
use crate::plugins::telemetry::config::TracingCommon;
use crate::plugins::telemetry::config_new::spans::Spans;
use crate::plugins::telemetry::endpoint::UriEndpoint;
use crate::plugins::telemetry::otel::layer::ORIGINAL_SPAN_NAME_FIELD;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::tracing::SpanProcessorExt;
use crate::plugins::telemetry::tracing::TracingConfigurator;

fn default_resource_mappings() -> HashMap<String, String> {
    let mut map = HashMap::new();
    map.insert("request", "http.route");
    map.insert("router", "http.route");
    map.insert("supergraph", "graphql.operation.name");
    map.insert("query_planning", "graphql.operation.name");
    map.insert("subgraph", "subgraph.name");
    map.insert("subgraph_request", "graphql.operation.name");
    map.insert("http_request", "http.route");
    map.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8126";

const BUILT_IN_SPAN_NAMES: [&str; 3] = ["router", "supergraph", "subgraph"];

#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// Enable datadog
    pub(crate) enabled: bool,

    /// The endpoint to send to
    #[serde(default)]
    pub(crate) endpoint: UriEndpoint,

    /// batch processor configuration
    #[serde(default)]
    pub(crate) batch_processor: BatchProcessorConfig,

    /// Enable datadog span mapping for span name and resource name.
    #[serde(default)]
    pub(crate) enable_span_mapping: bool,

    /// Custom mapping to be used as the resource field in spans, defaults to:
    /// router -> http.route
    /// supergraph -> graphql.operation.name
    /// subgraph -> subgraph.name
    /// subgraph_request -> subgraph.name
    /// http_request -> http.route
    #[serde(default)]
    resource_mapping: HashMap<String, String>,
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

        let exporter = opentelemetry_datadog::new_pipeline()
            .with(
                &self.endpoint.to_uri(&Uri::from_static(DEFAULT_ENDPOINT)),
                |builder, e| builder.with_agent_endpoint(e.to_string().trim_end_matches('/')),
            )
            .with(&resource_mappings, |builder, resource_mappings| {
                let resource_mappings = resource_mappings.clone();
                builder.with_resource_mapping(move |span, _model_config| {
                    let span_name = if let Some(original) = span
                        .attributes
                        .get(&Key::from_static_str(ORIGINAL_SPAN_NAME_FIELD))
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
            .with_name_mapping(|span, _model_config| {
                if let Some(original) = span
                    .attributes
                    .get(&Key::from_static_str(ORIGINAL_SPAN_NAME_FIELD))
                {
                    // Datadog expects static span names, not the ones in the otel spec.
                    // Remap the span name to the original name if it was remapped.
                    for name in BUILT_IN_SPAN_NAMES {
                        if name == original.as_str() {
                            return name;
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
            .with_version(
                common
                    .resource
                    .get(SERVICE_VERSION)
                    .expect("cargo version is set as a resource default;qed")
                    .to_string(),
            )
            .with_trace_config(common)
            .build_exporter()?;
        Ok(builder.with_span_processor(
            BatchSpanProcessor::builder(exporter, opentelemetry::runtime::Tokio)
                .with_batch_config(self.batch_processor.clone().into())
                .build()
                .filtered(),
        ))
    }
}
