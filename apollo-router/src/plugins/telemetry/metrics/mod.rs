use opentelemetry::metrics::{Counter, Meter, MeterProvider, Number, ValueRecorder};
use opentelemetry::KeyValue;
use std::sync::Arc;

pub mod otlp;
pub mod prometheus;

#[derive(Clone)]
pub struct BasicMetrics {
    pub http_requests_total: AggregateCounter<u64>,
    pub http_requests_error_total: AggregateCounter<u64>,
    pub http_requests_duration: AggregateValueRecorder<f64>,
}

impl BasicMetrics {
    pub fn new(meter_provider: &AggregateMeterProvider) -> BasicMetrics {
        let meter = meter_provider.meter("apollo/router", None);
        BasicMetrics {
            http_requests_total: meter.build_counter(|m| {
                m.u64_counter("http_requests_total")
                    .with_description("Total number of HTTP requests made.")
                    .init()
            }),
            http_requests_error_total: meter.build_counter(|m| {
                m.u64_counter("http_requests_error_total")
                    .with_description("Total number of HTTP requests in error made.")
                    .init()
            }),
            http_requests_duration: meter.build_value_recorder(|m| {
                m.f64_value_recorder("http_request_duration_seconds")
                    .with_description("Total number of HTTP requests made.")
                    .init()
            }),
        }
    }
}

#[derive(Clone, Default)]
pub struct AggregateMeterProvider(Vec<Arc<dyn MeterProvider + Send + Sync + 'static>>);
impl AggregateMeterProvider {
    pub fn new(
        meters: Vec<Arc<dyn MeterProvider + Send + Sync + 'static>>,
    ) -> AggregateMeterProvider {
        AggregateMeterProvider(meters)
    }

    pub fn meter(
        &self,
        instrumentation_name: &'static str,
        instrumentation_version: Option<&'static str>,
    ) -> AggregateMeter {
        AggregateMeter(
            self.0
                .iter()
                .map(|p| Arc::new(p.meter(instrumentation_name, instrumentation_version)))
                .collect(),
        )
    }
}

#[derive(Clone)]
pub struct AggregateMeter(Vec<Arc<Meter>>);
impl AggregateMeter {
    pub fn build_counter<T: Into<Number> + Copy>(
        &self,
        build: fn(&Meter) -> Counter<T>,
    ) -> AggregateCounter<T> {
        AggregateCounter(self.0.iter().map(|m| build(m)).collect())
    }

    pub fn build_value_recorder<T: Into<Number> + Copy>(
        &self,
        build: fn(&Meter) -> ValueRecorder<T>,
    ) -> AggregateValueRecorder<T> {
        AggregateValueRecorder(self.0.iter().map(|m| build(m)).collect())
    }
}

#[derive(Clone)]
pub struct AggregateCounter<T: Into<Number> + Copy>(Vec<Counter<T>>);
impl<T> AggregateCounter<T>
where
    T: Into<Number> + Copy,
{
    pub fn add(&self, value: T, attributes: &[KeyValue]) {
        for counter in &self.0 {
            counter.add(value, attributes)
        }
    }
}

#[derive(Clone)]
pub struct AggregateValueRecorder<T: Into<Number> + Copy>(Vec<ValueRecorder<T>>);
impl<T> AggregateValueRecorder<T>
where
    T: Into<Number> + Copy,
{
    pub fn record(&self, value: T, attributes: &[KeyValue]) {
        for counter in &self.0 {
            counter.record(value, attributes)
        }
    }
}
