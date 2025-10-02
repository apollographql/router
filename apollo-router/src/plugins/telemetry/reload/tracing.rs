use opentelemetry::propagation::TextMapCompositePropagator;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry_sdk::trace::SpanProcessor;
use opentelemetry_sdk::trace::TracerProvider;
use tower::BoxError;

use crate::plugins::telemetry::CustomTraceIdPropagator;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::config::Propagation;
use crate::plugins::telemetry::config::Tracing;
use crate::plugins::telemetry::config::TracingCommon;
use crate::plugins::telemetry::config_new::spans::Spans;

pub(crate) struct TracingBuilder<'a> {
    common: &'a TracingCommon,
    spans: &'a Spans,
    builder: opentelemetry_sdk::trace::Builder,
}

impl<'a> TracingBuilder<'a> {
    pub(crate) fn new(config: &'a Conf) -> Self {
        Self {
            common: &config.exporters.tracing.common,
            spans: &config.instrumentation.spans,
            builder: opentelemetry_sdk::trace::TracerProvider::builder()
                .with_config((&config.exporters.tracing.common).into()),
        }
    }

    pub(crate) fn configure<T: TracingConfigurator>(&mut self, config: &T) -> Result<(), BoxError> {
        if config.is_enabled() {
            return config.configure(self);
        }
        Ok(())
    }

    pub(crate) fn tracing_common(&self) -> &TracingCommon {
        self.common
    }

    pub(crate) fn spans(&self) -> &Spans {
        self.spans
    }

    pub(crate) fn with_span_processor<T: SpanProcessor + 'static>(&mut self, span_processor: T) {
        let builder = std::mem::take(&mut self.builder);
        self.builder = builder.with_span_processor(span_processor);
    }

    pub(crate) fn build(self) -> TracerProvider {
        self.builder.build()
    }
}

pub(crate) fn create_propagator(
    propagation: &Propagation,
    tracing: &Tracing,
) -> TextMapCompositePropagator {
    let mut propagators: Vec<Box<dyn TextMapPropagator + Send + Sync + 'static>> = Vec::new();
    if propagation.jaeger {
        propagators.push(Box::<opentelemetry_jaeger_propagator::Propagator>::default());
    }
    if propagation.baggage {
        propagators.push(Box::<opentelemetry_sdk::propagation::BaggagePropagator>::default());
    }
    if propagation.trace_context || tracing.otlp.enabled {
        propagators.push(Box::<opentelemetry_sdk::propagation::TraceContextPropagator>::default());
    }
    if propagation.zipkin || tracing.zipkin.enabled {
        propagators.push(Box::<opentelemetry_zipkin::Propagator>::default());
    }
    if propagation.datadog || tracing.datadog.enabled {
        propagators.push(Box::<
            crate::plugins::telemetry::tracing::datadog_exporter::DatadogPropagator,
        >::default());
    }
    if propagation.aws_xray {
        propagators.push(Box::<opentelemetry_aws::trace::XrayPropagator>::default());
    }

    // This propagator MUST come last because the user is trying to override the default behavior of the
    // other propagators.
    if let Some(from_request_header) = &propagation.request.header_name {
        propagators.push(Box::new(CustomTraceIdPropagator::new(
            from_request_header.to_string(),
            propagation.request.format.clone(),
        )));
    }
    TextMapCompositePropagator::new(propagators)
}

pub(crate) trait TracingConfigurator {
    fn config(conf: &Conf) -> &Self;
    fn is_enabled(&self) -> bool;
    fn configure(&self, builder: &mut TracingBuilder) -> Result<(), BoxError>;
}
