use crate::plugin::utils::serde::{deserialize_header_name, deserialize_regex};
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::metrics::apollo::Sender;
use crate::{http_compat, Handler, ResponseBody};
use ::serde::Deserialize;
use bytes::Bytes;
use http::header::HeaderName;
use opentelemetry::metrics::{Counter, Meter, MeterProvider, Number, ValueRecorder};
use opentelemetry::KeyValue;
use regex::Regex;
use schemars::JsonSchema;
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use tower::util::BoxService;
use tower::BoxError;

pub(crate) mod apollo;
pub(crate) mod otlp;
pub(crate) mod prometheus;

pub(crate) type MetricsExporterHandle = Box<dyn Any + Send + Sync + 'static>;
pub(crate) type CustomEndpoint =
    BoxService<http_compat::Request<Bytes>, http_compat::Response<ResponseBody>, BoxError>;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
/// Configuration to add custom attributes/labels on metrics
pub struct MetricsAttributesConf {
    /// Configuration to forward header values in metric attributes/labels
    pub(crate) from_headers: Option<Vec<Forward>>,
    /// Configuration to insert custom attributes/labels in metrics
    #[serde(rename = "static")]
    pub(crate) insert: Option<Vec<Insert>>,
}

#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
/// Configuration to insert custom attributes/labels in metrics
pub(crate) struct Insert {
    name: String,
    value: String,
}

#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[serde(untagged)]
/// Configuration to forward header values in metric labels
pub(crate) enum Forward {
    /// Using a named header
    Named {
        #[schemars(schema_with = "string_schema")]
        #[serde(deserialize_with = "deserialize_header_name")]
        named: HeaderName,
        rename: Option<String>,
        default: Option<String>,
    },
    /// Using a regex on the header name
    Matching {
        #[schemars(schema_with = "string_schema")]
        #[serde(deserialize_with = "deserialize_regex")]
        matching: Regex,
    },
}

fn string_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    String::json_schema(gen)
}

#[derive(Default)]
pub(crate) struct MetricsBuilder {
    exporters: Vec<MetricsExporterHandle>,
    meter_providers: Vec<Arc<dyn MeterProvider + Send + Sync + 'static>>,
    custom_endpoints: HashMap<String, Handler>,
    apollo_metrics: Sender,
}

impl MetricsBuilder {
    pub(crate) fn exporters(&mut self) -> Vec<MetricsExporterHandle> {
        std::mem::take(&mut self.exporters)
    }
    pub(crate) fn meter_provider(&mut self) -> AggregateMeterProvider {
        AggregateMeterProvider::new(std::mem::take(&mut self.meter_providers))
    }
    pub(crate) fn custom_endpoints(&mut self) -> HashMap<String, Handler> {
        std::mem::take(&mut self.custom_endpoints)
    }

    pub(crate) fn apollo_metrics_provider(&mut self) -> Sender {
        std::mem::take(&mut self.apollo_metrics)
    }
}

impl MetricsBuilder {
    fn with_exporter<T: Send + Sync + 'static>(mut self, handle: T) -> Self {
        self.exporters.push(Box::new(handle));
        self
    }

    fn with_meter_provider<T: MeterProvider + Send + Sync + 'static>(
        mut self,
        meter_provider: T,
    ) -> Self {
        self.meter_providers.push(Arc::new(meter_provider));
        self
    }

    fn with_custom_endpoint(mut self, path: &str, endpoint: CustomEndpoint) -> Self {
        self.custom_endpoints
            .insert(path.to_string(), Handler::new(endpoint));
        self
    }

    fn with_apollo_metrics_collector(mut self, apollo_metrics: Sender) -> Self {
        self.apollo_metrics = apollo_metrics;
        self
    }
}

pub(crate) trait MetricsConfigurator {
    fn apply(
        &self,
        builder: MetricsBuilder,
        metrics_config: &MetricsCommon,
    ) -> Result<MetricsBuilder, BoxError>;
}

#[derive(Clone)]
pub(crate) struct BasicMetrics {
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
pub(crate) struct AggregateMeterProvider(Vec<Arc<dyn MeterProvider + Send + Sync + 'static>>);
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
        for value_recorder in &self.0 {
            value_recorder.record(value, attributes)
        }
    }
}
