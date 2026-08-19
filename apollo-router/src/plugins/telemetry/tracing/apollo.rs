//! Tracing configuration for apollo telemetry.
use opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor;
use tower::BoxError;

use crate::plugins::telemetry::apollo::Config;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::metrics::BlockingSafeTokioRuntime;
use crate::plugins::telemetry::reload::tracing::TracingBuilder;
use crate::plugins::telemetry::reload::tracing::TracingConfigurator;
use crate::plugins::telemetry::tracing::NamedSpanExporter;
use crate::plugins::telemetry::tracing::SpanProcessorExt;
use crate::plugins::telemetry::tracing::apollo_telemetry;

impl TracingConfigurator for Config {
    fn config(conf: &Conf) -> &Self {
        &conf.apollo
    }

    fn is_enabled(&self) -> bool {
        self.apollo_key.is_some() && self.apollo_graph_ref.is_some()
    }

    fn configure(&self, builder: &mut TracingBuilder) -> Result<(), BoxError> {
        tracing::debug!("configuring Apollo tracing");
        let exporter = apollo_telemetry::Exporter::builder()
            .endpoint(&self.otlp_endpoint)
            .tracing_protocol(&self.otlp_tracing_protocol)
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
            .batch_processor_config(&self.tracing.batch_processor)
            .errors_configuration(&self.errors)
            .build()?;
        let named_exporter = NamedSpanExporter::new(exporter, "apollo");
        let batch_span_processor = BatchSpanProcessor::builder(
            named_exporter,
            BlockingSafeTokioRuntime::new_for_tracing("apollo-tracing"),
        )
        .with_batch_config(self.tracing.batch_processor.clone().into())
        .build();

        if let Some(sampler) = &self.sampler {
            let common = builder.tracing_common();
            let sampled_batch_span_processor = batch_span_processor.with_sampler(
                sampler,
                common.parent_based_sampler,
                &common.sampler,
            );

            builder.with_span_processor(sampled_batch_span_processor);
        } else {
            builder.with_span_processor(batch_span_processor);
        }
        Ok(())
    }
}
