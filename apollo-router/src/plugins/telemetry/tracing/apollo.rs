//! Tracing configuration for apollo telemetry.
use std::collections::HashMap;

// This entire file is license key functionality
use apollo_spaceport::Trace;
use opentelemetry::sdk::trace::Builder;
use serde::Serialize;
use tower::BoxError;

use crate::plugins::telemetry::apollo::Config;
use crate::plugins::telemetry::config;
use crate::plugins::telemetry::tracing::apollo_telemetry;
use crate::plugins::telemetry::tracing::TracingConfigurator;

impl TracingConfigurator for Config {
    fn apply(&self, builder: Builder, trace_config: &config::Trace) -> Result<Builder, BoxError> {
        tracing::debug!("configuring Apollo tracing");
        Ok(match self {
            Config {
                endpoint: Some(endpoint),
                apollo_key: Some(key),
                apollo_graph_ref: Some(reference),
                client_name_header,
                client_version_header,
                schema_id,
                apollo_sender,
            } => {
                tracing::debug!("configuring exporter to Spaceport");
                let exporter = apollo_telemetry::Exporter::builder()
                    .trace_config(trace_config.clone())
                    .endpoint(endpoint.clone())
                    .apollo_key(key)
                    .apollo_graph_ref(reference)
                    .client_name_header(client_name_header)
                    .client_version_header(client_version_header)
                    .schema_id(schema_id)
                    .apollo_sender(apollo_sender.clone())
                    .build();
                builder.with_batch_exporter(exporter, opentelemetry::runtime::Tokio)
            }
            _ => builder,
        })
    }
}

#[derive(Default, Debug, Serialize)]
pub(crate) struct SingleTracesReport {
    pub(crate) traces: HashMap<String, SingleTraces>,
}

#[derive(Default, Debug, Serialize, derive_more::From)]
pub(crate) struct SingleTraces {
    pub(crate) traces: Vec<Trace>,
}
