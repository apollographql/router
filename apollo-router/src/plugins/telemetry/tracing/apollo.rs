//! Tracing configuration for apollo telemetry.
// With regards to ELv2 licensing, this entire file is license key functionality
use opentelemetry::sdk::trace::Builder;
use serde::Serialize;
use tower::BoxError;

use crate::plugins::telemetry::apollo::Config;
use crate::plugins::telemetry::config;
use crate::plugins::telemetry::tracing::apollo_telemetry;
use crate::plugins::telemetry::tracing::TracingConfigurator;
use crate::spaceport::Trace;

impl TracingConfigurator for Config {
    fn apply(&self, builder: Builder, _trace_config: &config::Trace) -> Result<Builder, BoxError> {
        tracing::debug!("configuring Apollo tracing");
        Ok(match self {
            Config {
                endpoint: Some(endpoint),
                apollo_key: Some(key),
                apollo_graph_ref: Some(reference),
                schema_id,
                buffer_size,
                field_level_instrumentation_sampler,
                expose_trace_id,
                ..
            } => {
                tracing::debug!("configuring exporter to Studio");

                let exporter = apollo_telemetry::Exporter::builder()
                    .expose_trace_id_config(expose_trace_id.clone())
                    .endpoint(endpoint.clone())
                    .apollo_key(key)
                    .apollo_graph_ref(reference)
                    .schema_id(schema_id)
                    .buffer_size(*buffer_size)
                    .and_field_execution_sampler(field_level_instrumentation_sampler.clone())
                    .build()?;
                builder.with_batch_exporter(exporter, opentelemetry::runtime::Tokio)
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
