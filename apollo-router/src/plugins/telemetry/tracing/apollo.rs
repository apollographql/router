//! Tracing configuration for apollo telemetry.
// This entire file is license key functionality
use opentelemetry::sdk::trace::Builder;
use tower::BoxError;

use crate::plugins::telemetry::apollo::Config;
use crate::plugins::telemetry::config::Trace;
use crate::plugins::telemetry::tracing::apollo_telemetry;
use crate::plugins::telemetry::tracing::apollo_telemetry::SpaceportConfig;
use crate::plugins::telemetry::tracing::apollo_telemetry::StudioGraph;
use crate::plugins::telemetry::tracing::TracingConfigurator;

impl TracingConfigurator for Config {
    fn apply(&self, builder: Builder, trace_config: &Trace) -> Result<Builder, BoxError> {
        tracing::debug!("configuring Apollo tracing");
        Ok(match self {
            Config {
                endpoint: Some(endpoint),
                apollo_key: Some(key),
                apollo_graph_ref: Some(reference),
                ..
            } => {
                tracing::debug!("configuring exporter to Spaceport");
                let exporter = apollo_telemetry::new_pipeline()
                    .with_trace_config(trace_config.into())
                    .with_graph_config(&Some(StudioGraph {
                        reference: reference.clone(),
                        key: key.clone(),
                    }))
                    .with_spaceport_config(&Some(SpaceportConfig {
                        collector: endpoint.to_string(),
                    }))
                    .build_exporter()?;
                builder.with_batch_exporter(exporter, opentelemetry::runtime::Tokio)
            }
            _ => builder,
        })
    }
}
