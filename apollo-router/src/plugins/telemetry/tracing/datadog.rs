//! Configuration for datadog tracing.

use std::collections::HashMap;

use http::Uri;
use lazy_static::lazy_static;
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
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::tracing::SpanProcessorExt;
use crate::plugins::telemetry::tracing::TracingConfigurator;

lazy_static! {
    static ref SPAN_RESOURCE_NAME_ATTRIBUTE_MAPPING: HashMap<&'static str, &'static str> = {
        let mut map = HashMap::new();
        map.insert("request", "http.route");
        map.insert("supergraph", "graphql.operation.name");
        map.insert("query_planning", "graphql.operation.name");
        map.insert("subgraph", "subgraph.name");
        map.insert("subgraph_request", "graphql.operation.name");
        map
    };
    static ref DEFAULT_ENDPOINT: Uri = Uri::from_static("http://127.0.0.1:8126");
}

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
        let enable_span_mapping = self.enable_span_mapping.then_some(true);
        let common: sdk::trace::Config = trace.into();
        let exporter = opentelemetry_datadog::new_pipeline()
            .with(&self.endpoint.to_uri(&DEFAULT_ENDPOINT), |builder, e| {
                builder.with_agent_endpoint(e.to_string().trim_end_matches('/'))
            })
            .with(&enable_span_mapping, |builder, _e| {
                builder
                    .with_name_mapping(|span, _model_config| span.name.as_ref())
                    .with_resource_mapping(|span, _model_config| {
                        SPAN_RESOURCE_NAME_ATTRIBUTE_MAPPING
                            .get(span.name.as_ref())
                            .and_then(|key| span.attributes.get(&Key::from_static_str(key)))
                            .and_then(|value| match value {
                                Value::String(value) => Some(value.as_str()),
                                _ => None,
                            })
                            .unwrap_or(span.name.as_ref())
                    })
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
