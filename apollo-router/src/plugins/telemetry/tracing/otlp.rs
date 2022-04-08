use crate::plugins::telemetry::config::Trace;
use crate::plugins::telemetry::tracing::TracingConfigurator;
use opentelemetry::sdk::trace::Builder;
use opentelemetry_otlp::SpanExporterBuilder;
use std::result::Result;
use tower::BoxError;

impl TracingConfigurator for super::super::otlp::Config {
    fn apply(&self, builder: Builder, _trace_config: &Trace) -> Result<Builder, BoxError> {
        tracing::debug!("configuring Otlp tracing");
        let exporter: SpanExporterBuilder = self.exporter()?;
        Ok(builder.with_batch_exporter(
            exporter.build_span_exporter()?,
            opentelemetry::runtime::Tokio,
        ))
    }
}

#[cfg(test)]
mod tests {
    use crate::plugins::telemetry::tracing::test::run_query;
    use opentelemetry::global;
    use opentelemetry::sdk::propagation::TraceContextPropagator;
    use tower::BoxError;
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Registry;

    // This test can be run manually from your IDE to help with testing otel
    // It is set to ignore by default as otlp may not be set up
    #[ignore]
    #[tokio::test(flavor = "multi_thread")]
    async fn test_tracing() -> Result<(), BoxError> {
        tracing_subscriber::fmt().init();

        global::set_text_map_propagator(TraceContextPropagator::new());
        let tracer = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(opentelemetry_otlp::new_exporter().http())
            .install_batch(opentelemetry::runtime::Tokio)?;

        // Create a tracing layer with the configured tracer
        let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

        // Use the tracing subscriber `Registry`, or any other subscriber
        // that impls `LookupSpan`
        let subscriber = Registry::default().with(telemetry);

        // Trace executed code
        run_query().with_subscriber(subscriber).await;
        global::shutdown_tracer_provider();

        Ok(())
    }
}
