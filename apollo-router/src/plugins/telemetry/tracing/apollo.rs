//! Tracing configuration for apollo telemetry.
use opentelemetry::sdk::trace::BatchSpanProcessor;
use opentelemetry::sdk::trace::Builder;
use serde::Serialize;
use tower::BoxError;

use crate::plugins::telemetry::apollo::Config;
use crate::plugins::telemetry::apollo_exporter::proto::reports::Trace;
use crate::plugins::telemetry::config;
use crate::plugins::telemetry::tracing::apollo_telemetry;
use crate::plugins::telemetry::tracing::TracingConfigurator;

impl TracingConfigurator for Config {
    fn apply(&self, builder: Builder, _trace_config: &config::Trace) -> Result<Builder, BoxError> {
        tracing::debug!("configuring Apollo tracing");
        Ok(match self {
            Config {
                endpoint,
                apollo_key: Some(key),
                apollo_graph_ref: Some(reference),
                schema_id,
                buffer_size,
                field_level_instrumentation_sampler,
                batch_processor,
                errors,
                ..
            } => {
                tracing::debug!("configuring exporter to Studio");

                let exporter = apollo_telemetry::Exporter::builder()
                    .endpoint(endpoint.clone())
                    .apollo_key(key)
                    .apollo_graph_ref(reference)
                    .schema_id(schema_id)
                    .buffer_size(*buffer_size)
                    .field_execution_sampler(field_level_instrumentation_sampler.clone())
                    .batch_config(batch_processor.clone())
                    .errors_configuration(errors.clone())
                    .build()?;
                builder.with_span_processor(
                    BatchSpanProcessor::builder(exporter, opentelemetry::runtime::Tokio)
                        .with_batch_config(batch_processor.clone().into())
                        .build(),
                )
            }
            _ => builder,
        })
    }
}

// List of signature and trace by request_id
#[derive(Default, Debug, Serialize)]
pub(crate) struct TracesReport {
    // signature and trace
    pub(crate) traces: Vec<(String, Trace)>,
}
