//! Tracing configuration for apollo telemetry.
use opentelemetry::sdk::trace::BatchSpanProcessor;
use opentelemetry::sdk::trace::Builder;
use serde::Serialize;
use tower::BoxError;

use crate::plugins::telemetry::apollo::Config;
use crate::plugins::telemetry::apollo_exporter::proto::reports::Trace;
use crate::plugins::telemetry::config;
use crate::plugins::telemetry::config_new::spans::Spans;
use crate::plugins::telemetry::span_factory::SpanMode;
use crate::plugins::telemetry::tracing::apollo_telemetry;
use crate::plugins::telemetry::tracing::TracingConfigurator;

impl TracingConfigurator for Config {
    fn enabled(&self) -> bool {
        self.apollo_key.is_some() && self.apollo_graph_ref.is_some()
    }

    fn apply(
        &self,
        builder: Builder,
        _common: &config::TracingCommon,
        spans_config: &Spans,
    ) -> Result<Builder, BoxError> {
        tracing::debug!("configuring Apollo tracing");
        let exporter = apollo_telemetry::Exporter::builder()
            .endpoint(&self.endpoint)
            .apollo_key(
                self.apollo_key
                    .as_ref()
                    .expect("apollo_key is checked in the enabled function, qed"),
            )
            .apollo_graph_ref(
                self.apollo_graph_ref
                    .as_ref()
                    .expect("apollo_graph_ref is checked in the enabled function, qed"),
            )
            .schema_id(&self.schema_id)
            .buffer_size(self.buffer_size)
            .field_execution_sampler(&self.field_level_instrumentation_sampler)
            .batch_config(&self.batch_processor)
            .errors_configuration(&self.errors)
            .use_legacy_request_span(matches!(spans_config.mode, SpanMode::Deprecated))
            .build()?;
        Ok(builder.with_span_processor(
            BatchSpanProcessor::builder(exporter, opentelemetry::runtime::Tokio)
                .with_batch_config(self.batch_processor.clone().into())
                .build(),
        ))
    }
}

// List of signature and trace by request_id
#[derive(Default, Debug, Serialize)]
pub(crate) struct TracesReport {
    // signature and trace
    pub(crate) traces: Vec<(String, Trace)>,
}
