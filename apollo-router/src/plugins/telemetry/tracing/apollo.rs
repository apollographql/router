//! Tracing configuration for apollo telemetry.
use std::collections::HashMap;

// This entire file is license key functionality
use apollo_spaceport::Trace;
use opentelemetry::sdk::trace::Builder;
use serde::Serialize;
use tower::BoxError;

use crate::plugins::telemetry::apollo::Config;
use crate::plugins::telemetry::apollo::ForwardValues;
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
                buffer_size,
                field_level_instrumentation,
                send_headers,
                send_variable_values,
            } => {
                tracing::debug!("configuring exporter to Spaceport");

                let mut send_headers = send_headers.clone();
                match &mut send_headers {
                    ForwardValues::None | ForwardValues::All => {}
                    ForwardValues::Only(headers) | ForwardValues::Except(headers) => headers
                        .iter_mut()
                        .for_each(|header_name| *header_name = header_name.to_lowercase()),
                };

                let exporter = apollo_telemetry::Exporter::builder()
                    .trace_config(trace_config.clone())
                    .endpoint(endpoint.clone())
                    .apollo_key(key)
                    .apollo_graph_ref(reference)
                    .client_name_header(client_name_header)
                    .client_version_header(client_version_header)
                    .schema_id(schema_id)
                    .apollo_sender(apollo_sender.clone())
                    .buffer_size(*buffer_size)
                    .field_level_instrumentation(*field_level_instrumentation)
                    .send_headers(send_headers)
                    .send_variable_values(send_variable_values.clone())
                    .build();
                builder.with_batch_exporter(exporter, opentelemetry::runtime::Tokio)
            }
            _ => builder,
        })
    }
}

// List of signature and trace by request_id
#[derive(Default, Debug, Serialize)]
pub(crate) struct TracesReport {
    // signature and trace by request_id
    pub(crate) traces: HashMap<String, (String, Trace)>,
}
