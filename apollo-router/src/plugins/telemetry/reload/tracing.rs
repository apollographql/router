//! Trace provider construction
//!
//! This module provides tools for building OpenTelemetry tracer providers from router configuration.
//!
//! ## Purpose
//!
//! The [`TracingBuilder`] constructs a tracer provider that handles distributed tracing across
//! multiple backends (OTLP, Datadog, Zipkin, Apollo). It also configures trace propagation to
//! ensure trace context is properly propagated across service boundaries.
//!
//! ## Configurator Pattern
//!
//! The [`TracingConfigurator`] trait allows different trace exporters to contribute span processors
//! to the builder. Each exporter (OTLP, Datadog, Zipkin, Apollo) implements this trait to add its
//! specific span processing logic.
//!
//! ## Propagation
//!
//! The [`create_propagator`] function builds a composite propagator supporting multiple trace
//! context formats (W3C Trace Context, Jaeger, Zipkin, Datadog, AWS X-Ray). This allows the router
//! to interoperate with services using different tracing systems.

use opentelemetry::propagation::TextMapCompositePropagator;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider, SpanProcessor, TracerProviderBuilder};
use tower::BoxError;
use crate::_private::telemetry::ConfigResource;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::config::Propagation;
use crate::plugins::telemetry::config::Tracing;
use crate::plugins::telemetry::config::TracingCommon;
use crate::plugins::telemetry::config_new::spans::Spans;
use crate::plugins::telemetry::CustomTraceIdPropagator;

/// Builder for constructing OpenTelemetry tracer providers with multiple exporters
pub(crate) struct TracingBuilder<'a> {
    common: &'a TracingCommon,
    spans: &'a Spans,
    builder: TracerProviderBuilder
}

impl<'a> TracingBuilder<'a> {
    pub(crate) fn new(config: &'a Conf) -> Self {
        Self {
            common: &config.exporters.tracing.common,
            spans: &config.instrumentation.spans,
            builder: opentelemetry_sdk::trace::SdkTracerProvider::builder()
                .with_resource(config.exporters.tracing.common.to_resource())
                .with_sampler::<Sampler>(config.exporters.tracing.common.sampler.clone().into())
                .with_max_events_per_span(config.exporters.tracing.common.max_events_per_span)
                .with_max_attributes_per_span(config.exporters.tracing.common.max_attributes_per_span)
                .with_max_attributes_per_event(config.exporters.tracing.common.max_attributes_per_event)
                .with_max_attributes_per_link(config.exporters.tracing.common.max_attributes_per_link),
                //TODO DD agent sampling
                //TODO parent_based_sampler
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

    pub(crate) fn build(self) -> SdkTracerProvider {
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
        propagators.push(Box::<opentelemetry_datadog::DatadogPropagator,>::default());
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

/// Trait for trace exporters to contribute to tracer provider construction
pub(crate) trait TracingConfigurator {
    fn config(conf: &Conf) -> &Self;
    fn is_enabled(&self) -> bool;
    fn configure(&self, builder: &mut TracingBuilder) -> Result<(), BoxError>;
}
