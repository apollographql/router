//! Configuration for the telemetry plugin.
use std::collections::BTreeMap;
use std::collections::HashSet;

use axum::headers::HeaderName;
use opentelemetry::sdk::metrics::new_view;
use opentelemetry::sdk::metrics::Aggregation;
use opentelemetry::sdk::metrics::Instrument;
use opentelemetry::sdk::metrics::Stream;
use opentelemetry::sdk::metrics::View;
use opentelemetry::sdk::trace::SpanLimits;
use opentelemetry::Array;
use opentelemetry::Value;
use opentelemetry_api::metrics::MetricsError;
use opentelemetry_api::metrics::Unit;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use super::metrics::MetricsAttributesConf;
use super::*;
use crate::plugin::serde::deserialize_option_header_name;
use crate::plugins::telemetry::metrics;
use crate::plugins::telemetry::resource::ConfigResource;

#[derive(thiserror::Error, Debug)]
pub(crate) enum Error {
    #[error("field level instrumentation sampler must sample less frequently than tracing level sampler")]
    InvalidFieldLevelInstrumentationSampler,
}

pub(in crate::plugins::telemetry) trait GenericWith<T>
where
    Self: Sized,
{
    fn with<B>(self, option: &Option<B>, apply: fn(Self, &B) -> Self) -> Self {
        if let Some(option) = option {
            return apply(self, option);
        }
        self
    }
    fn try_with<B>(
        self,
        option: &Option<B>,
        apply: fn(Self, &B) -> Result<Self, BoxError>,
    ) -> Result<Self, BoxError> {
        if let Some(option) = option {
            return apply(self, option);
        }
        Ok(self)
    }
}

impl<T> GenericWith<T> for T where Self: Sized {}

/// Telemetry configuration
#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Conf {
    /// Apollo reporting configuration
    pub(crate) apollo: apollo::Config,

    /// Instrumentation configuration
    pub(crate) exporters: Exporters,

    /// Instrumentation configuration
    pub(crate) instrumentation: Instrumentation,
}

/// Exporter configuration
#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Exporters {
    /// Logging configuration
    pub(crate) logging: config_new::logging::Logging,
    /// Metrics configuration
    pub(crate) metrics: Metrics,
    /// Tracing configuration
    pub(crate) tracing: Tracing,
}

/// Instrumentation configuration
#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Instrumentation {
    /// Event configuration
    pub(crate) events: config_new::events::Events,
    /// Span configuration
    pub(crate) spans: config_new::spans::Spans,
    /// Instrument configuration
    pub(crate) instruments: config_new::instruments::InstrumentsConfig,
}

/// Metrics configuration
#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Metrics {
    /// Common metrics configuration across all exporters
    pub(crate) common: MetricsCommon,
    /// Open Telemetry native exporter configuration
    pub(crate) otlp: otlp::Config,
    /// Prometheus exporter configuration
    pub(crate) prometheus: metrics::prometheus::Config,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct MetricsCommon {
    /// Configuration to add custom labels/attributes to metrics
    pub(crate) attributes: MetricsAttributesConf,
    /// Set a service.name resource in your metrics
    pub(crate) service_name: Option<String>,
    /// Set a service.namespace attribute in your metrics
    pub(crate) service_namespace: Option<String>,
    /// The Open Telemetry resource
    pub(crate) resource: BTreeMap<String, AttributeValue>,
    /// Custom buckets for all histograms
    pub(crate) buckets: Vec<f64>,
    /// Views applied on metrics
    pub(crate) views: Vec<MetricView>,
}

impl Default for MetricsCommon {
    fn default() -> Self {
        Self {
            attributes: Default::default(),
            service_name: None,
            service_namespace: None,
            resource: BTreeMap::new(),
            views: Vec::with_capacity(0),
            buckets: vec![
                0.001, 0.005, 0.015, 0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 1.0, 5.0, 10.0,
            ],
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MetricView {
    /// The instrument name you're targeting
    pub(crate) name: String,
    /// New description to set to the instrument
    pub(crate) description: Option<String>,
    /// New unit to set to the instrument
    pub(crate) unit: Option<String>,
    /// New aggregation settings to set
    pub(crate) aggregation: Option<MetricAggregation>,
    /// An allow-list of attribute keys that will be preserved for the instrument.
    ///
    /// Any attribute recorded for the instrument with a key not in this set will be
    /// dropped. If the set is empty, all attributes will be dropped, if `None` all
    /// attributes will be kept.
    pub(crate) allowed_attribute_keys: Option<HashSet<String>>,
}

impl TryInto<Box<dyn View>> for MetricView {
    type Error = MetricsError;

    fn try_into(self) -> Result<Box<dyn View>, Self::Error> {
        let aggregation = self
            .aggregation
            .map(
                |MetricAggregation::Histogram { buckets }| Aggregation::ExplicitBucketHistogram {
                    boundaries: buckets,
                    record_min_max: true,
                },
            );
        let instrument = Instrument::new().name(self.name);
        let mut mask = Stream::new();
        if let Some(desc) = self.description {
            mask = mask.description(desc);
        }
        if let Some(unit) = self.unit {
            mask = mask.unit(Unit::new(unit));
        }
        if let Some(aggregation) = aggregation {
            mask = mask.aggregation(aggregation);
        }
        if let Some(allowed_attribute_keys) = self.allowed_attribute_keys {
            mask = mask.allowed_attribute_keys(allowed_attribute_keys.into_iter().map(Key::new));
        }

        new_view(instrument, mask)
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum MetricAggregation {
    /// An aggregation that summarizes a set of measurements as an histogram with
    /// explicitly defined buckets.
    Histogram { buckets: Vec<f64> },
}

/// Tracing configuration
#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Tracing {
    // TODO: when deleting the `experimental_` prefix, check the usage when enabling dev mode
    // When deleting, put a #[serde(alias = "experimental_response_trace_id")] if we don't want to break things
    /// A way to expose trace id in response headers
    #[serde(default, rename = "experimental_response_trace_id")]
    pub(crate) response_trace_id: ExposeTraceId,
    /// Propagation configuration
    pub(crate) propagation: Propagation,
    /// Common configuration
    pub(crate) common: TracingCommon,
    /// OpenTelemetry native exporter configuration
    pub(crate) otlp: otlp::Config,
    /// Jaeger exporter configuration
    pub(crate) jaeger: tracing::jaeger::Config,
    /// Zipkin exporter configuration
    pub(crate) zipkin: tracing::zipkin::Config,
    /// Datadog exporter configuration
    pub(crate) datadog: tracing::datadog::Config,
}

#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ExposeTraceId {
    /// Expose the trace_id in response headers
    pub(crate) enabled: bool,
    /// Choose the header name to expose trace_id (default: apollo-trace-id)
    #[schemars(with = "Option<String>")]
    #[serde(deserialize_with = "deserialize_option_header_name")]
    pub(crate) header_name: Option<HeaderName>,
    /// Format of the trace ID in response headers
    pub(crate) format: TraceIdFormat,
}

#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub(crate) enum TraceIdFormat {
    /// Format the Trace ID as a hexadecimal number
    ///
    /// (e.g. Trace ID 16 -> 00000000000000000000000000000010)
    #[default]
    Hexadecimal,
    /// Format the Trace ID as a decimal number
    ///
    /// (e.g. Trace ID 16 -> 16)
    Decimal,
}

/// Configure propagation of traces. In general you won't have to do this as these are automatically configured
/// along with any exporter you configure.
#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Propagation {
    /// Select a custom request header to set your own trace_id (header value must be convertible from hexadecimal to set a correct trace_id)
    pub(crate) request: RequestPropagation,
    /// Propagate baggage https://www.w3.org/TR/baggage/
    pub(crate) baggage: bool,
    /// Propagate trace context https://www.w3.org/TR/trace-context/
    pub(crate) trace_context: bool,
    /// Propagate Jaeger
    pub(crate) jaeger: bool,
    /// Propagate Datadog
    pub(crate) datadog: bool,
    /// Propagate Zipkin
    pub(crate) zipkin: bool,
    /// Propagate AWS X-Ray
    pub(crate) aws_xray: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestPropagation {
    /// Choose the header name to expose trace_id (default: apollo-trace-id)
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_option_header_name")]
    pub(crate) header_name: Option<HeaderName>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub(crate) struct TracingCommon {
    /// The trace service name
    pub(crate) service_name: Option<String>,
    /// The trace service namespace
    pub(crate) service_namespace: Option<String>,
    /// The sampler, always_on, always_off or a decimal between 0.0 and 1.0
    pub(crate) sampler: SamplerOption,
    /// Whether to use parent based sampling
    pub(crate) parent_based_sampler: bool,
    /// The maximum events per span before discarding
    pub(crate) max_events_per_span: u32,
    /// The maximum attributes per span before discarding
    pub(crate) max_attributes_per_span: u32,
    /// The maximum links per span before discarding
    pub(crate) max_links_per_span: u32,
    /// The maximum attributes per event before discarding
    pub(crate) max_attributes_per_event: u32,
    /// The maximum attributes per link before discarding
    pub(crate) max_attributes_per_link: u32,
    /// The Open Telemetry resource
    pub(crate) resource: BTreeMap<String, AttributeValue>,
}

impl ConfigResource for TracingCommon {
    fn service_name(&self) -> &Option<String> {
        &self.service_name
    }
    fn service_namespace(&self) -> &Option<String> {
        &self.service_namespace
    }
    fn resource(&self) -> &BTreeMap<String, AttributeValue> {
        &self.resource
    }
}

impl ConfigResource for MetricsCommon {
    fn service_name(&self) -> &Option<String> {
        &self.service_name
    }
    fn service_namespace(&self) -> &Option<String> {
        &self.service_namespace
    }
    fn resource(&self) -> &BTreeMap<String, AttributeValue> {
        &self.resource
    }
}

fn default_parent_based_sampler() -> bool {
    true
}

fn default_sampler() -> SamplerOption {
    SamplerOption::Always(Sampler::AlwaysOn)
}

impl Default for TracingCommon {
    fn default() -> Self {
        Self {
            service_name: Default::default(),
            service_namespace: Default::default(),
            sampler: default_sampler(),
            parent_based_sampler: default_parent_based_sampler(),
            max_events_per_span: default_max_events_per_span(),
            max_attributes_per_span: default_max_attributes_per_span(),
            max_links_per_span: default_max_links_per_span(),
            max_attributes_per_event: default_max_attributes_per_event(),
            max_attributes_per_link: default_max_attributes_per_link(),
            resource: Default::default(),
        }
    }
}

fn default_max_events_per_span() -> u32 {
    SpanLimits::default().max_events_per_span
}
fn default_max_attributes_per_span() -> u32 {
    SpanLimits::default().max_attributes_per_span
}
fn default_max_links_per_span() -> u32 {
    SpanLimits::default().max_links_per_span
}
fn default_max_attributes_per_event() -> u32 {
    SpanLimits::default().max_attributes_per_event
}
fn default_max_attributes_per_link() -> u32 {
    SpanLimits::default().max_attributes_per_link
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(untagged, deny_unknown_fields)]
pub(crate) enum AttributeValue {
    /// bool values
    Bool(bool),
    /// i64 values
    I64(i64),
    /// f64 values
    F64(f64),
    /// String values
    String(String),
    /// Array of homogeneous values
    Array(AttributeArray),
}

impl From<&'static str> for AttributeValue {
    fn from(value: &'static str) -> Self {
        AttributeValue::String(value.to_string())
    }
}

impl From<i64> for AttributeValue {
    fn from(value: i64) -> Self {
        AttributeValue::I64(value)
    }
}

impl std::fmt::Display for AttributeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AttributeValue::Bool(val) => write!(f, "{val}"),
            AttributeValue::I64(val) => write!(f, "{val}"),
            AttributeValue::F64(val) => write!(f, "{val}"),
            AttributeValue::String(val) => write!(f, "{val}"),
            AttributeValue::Array(val) => write!(f, "{val}"),
        }
    }
}

impl TryFrom<serde_json::Value> for AttributeValue {
    type Error = ();
    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        match value {
            serde_json::Value::Null => Err(()),
            serde_json::Value::Bool(v) => Ok(AttributeValue::Bool(v)),
            serde_json::Value::Number(v) if v.is_i64() => {
                Ok(AttributeValue::I64(v.as_i64().expect("i64 checked")))
            }
            serde_json::Value::Number(v) if v.is_f64() => {
                Ok(AttributeValue::F64(v.as_f64().expect("f64 checked")))
            }
            serde_json::Value::String(v) => Ok(AttributeValue::String(v)),
            serde_json::Value::Array(v) => {
                if v.iter().all(|v| v.is_boolean()) {
                    Ok(AttributeValue::Array(AttributeArray::Bool(
                        v.iter()
                            .map(|v| v.as_bool().expect("all bools checked"))
                            .collect(),
                    )))
                } else if v.iter().all(|v| v.is_f64()) {
                    Ok(AttributeValue::Array(AttributeArray::F64(
                        v.iter()
                            .map(|v| v.as_f64().expect("all f64 checked"))
                            .collect(),
                    )))
                } else if v.iter().all(|v| v.is_i64()) {
                    Ok(AttributeValue::Array(AttributeArray::I64(
                        v.iter()
                            .map(|v| v.as_i64().expect("all i64 checked"))
                            .collect(),
                    )))
                } else if v.iter().all(|v| v.is_string()) {
                    Ok(AttributeValue::Array(AttributeArray::String(
                        v.iter()
                            .map(|v| v.as_str().expect("all strings checked").to_string())
                            .collect(),
                    )))
                } else {
                    Err(())
                }
            }
            serde_json::Value::Object(_v) => Err(()),
            _ => Err(()),
        }
    }
}

impl From<AttributeValue> for opentelemetry::Value {
    fn from(value: AttributeValue) -> Self {
        match value {
            AttributeValue::Bool(v) => Value::Bool(v),
            AttributeValue::I64(v) => Value::I64(v),
            AttributeValue::F64(v) => Value::F64(v),
            AttributeValue::String(v) => Value::String(v.into()),
            AttributeValue::Array(v) => Value::Array(v.into()),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(untagged, deny_unknown_fields)]
pub(crate) enum AttributeArray {
    /// Array of bools
    Bool(Vec<bool>),
    /// Array of integers
    I64(Vec<i64>),
    /// Array of floats
    F64(Vec<f64>),
    /// Array of strings
    String(Vec<String>),
}

impl std::fmt::Display for AttributeArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AttributeArray::Bool(val) => write!(f, "{val:?}"),
            AttributeArray::I64(val) => write!(f, "{val:?}"),
            AttributeArray::F64(val) => write!(f, "{val:?}"),
            AttributeArray::String(val) => write!(f, "{val:?}"),
        }
    }
}

impl From<AttributeArray> for opentelemetry::Array {
    fn from(array: AttributeArray) -> Self {
        match array {
            AttributeArray::Bool(v) => Array::Bool(v),
            AttributeArray::I64(v) => Array::I64(v),
            AttributeArray::F64(v) => Array::F64(v),
            AttributeArray::String(v) => Array::String(v.into_iter().map(|v| v.into()).collect()),
        }
    }
}

impl From<opentelemetry::Array> for AttributeArray {
    fn from(array: opentelemetry::Array) -> Self {
        match array {
            opentelemetry::Array::Bool(v) => AttributeArray::Bool(v),
            opentelemetry::Array::I64(v) => AttributeArray::I64(v),
            opentelemetry::Array::F64(v) => AttributeArray::F64(v),
            opentelemetry::Array::String(v) => {
                AttributeArray::String(v.into_iter().map(|v| v.into()).collect())
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum SamplerOption {
    /// Sample a given fraction. Fractions >= 1 will always sample.
    TraceIdRatioBased(f64),
    Always(Sampler),
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Sampler {
    /// Always sample
    AlwaysOn,
    /// Never sample
    AlwaysOff,
}

impl From<Sampler> for opentelemetry::sdk::trace::Sampler {
    fn from(s: Sampler) -> Self {
        match s {
            Sampler::AlwaysOn => opentelemetry::sdk::trace::Sampler::AlwaysOn,
            Sampler::AlwaysOff => opentelemetry::sdk::trace::Sampler::AlwaysOff,
        }
    }
}

impl From<SamplerOption> for opentelemetry::sdk::trace::Sampler {
    fn from(s: SamplerOption) -> Self {
        match s {
            SamplerOption::Always(s) => s.into(),
            SamplerOption::TraceIdRatioBased(ratio) => {
                opentelemetry::sdk::trace::Sampler::TraceIdRatioBased(ratio)
            }
        }
    }
}

impl From<&TracingCommon> for opentelemetry::sdk::trace::Config {
    fn from(config: &TracingCommon) -> Self {
        let mut common = opentelemetry::sdk::trace::config();

        let mut sampler: opentelemetry::sdk::trace::Sampler = config.sampler.clone().into();
        if config.parent_based_sampler {
            sampler = parent_based(sampler);
        }

        common = common.with_sampler(sampler);
        common = common.with_max_events_per_span(config.max_events_per_span);
        common = common.with_max_attributes_per_span(config.max_attributes_per_span);
        common = common.with_max_links_per_span(config.max_links_per_span);
        common = common.with_max_attributes_per_event(config.max_attributes_per_event);
        common = common.with_max_attributes_per_link(config.max_attributes_per_link);

        // Take the default first, then config, then env resources, then env variable. Last entry wins
        common = common.with_resource(config.to_resource());
        common
    }
}

fn parent_based(sampler: opentelemetry::sdk::trace::Sampler) -> opentelemetry::sdk::trace::Sampler {
    opentelemetry::sdk::trace::Sampler::ParentBased(Box::new(sampler))
}

impl Conf {
    pub(crate) fn calculate_field_level_instrumentation_ratio(&self) -> Result<f64, Error> {
        Ok(
            match (
                &self.exporters.tracing.common.sampler,
                &self.apollo.field_level_instrumentation_sampler,
            ) {
                // Error conditions
                (
                    SamplerOption::TraceIdRatioBased(global_ratio),
                    SamplerOption::TraceIdRatioBased(field_ratio),
                ) if field_ratio > global_ratio => {
                    Err(Error::InvalidFieldLevelInstrumentationSampler)?
                }
                (
                    SamplerOption::Always(Sampler::AlwaysOff),
                    SamplerOption::Always(Sampler::AlwaysOn),
                ) => Err(Error::InvalidFieldLevelInstrumentationSampler)?,
                (
                    SamplerOption::Always(Sampler::AlwaysOff),
                    SamplerOption::TraceIdRatioBased(ratio),
                ) if *ratio != 0.0 => Err(Error::InvalidFieldLevelInstrumentationSampler)?,
                (
                    SamplerOption::TraceIdRatioBased(ratio),
                    SamplerOption::Always(Sampler::AlwaysOn),
                ) if *ratio != 1.0 => Err(Error::InvalidFieldLevelInstrumentationSampler)?,

                // Happy paths
                (_, SamplerOption::TraceIdRatioBased(ratio)) if *ratio == 0.0 => 0.0,
                (SamplerOption::TraceIdRatioBased(ratio), _) if *ratio == 0.0 => 0.0,
                (_, SamplerOption::Always(Sampler::AlwaysOn)) => 1.0,
                // the `field_ratio` should be a ratio of the entire set of requests. But FTV1 would only be reported
                // if a trace was generated with the Apollo exporter, which has its own sampling `global_ratio`.
                // in telemetry::request_ftv1, we activate FTV1 if the current trace is sampled and depending on
                // the ratio returned by this function.
                // This means that:
                // - field_ratio cannot be larger than global_ratio (see above, we return an error in that case)
                // - we have to divide field_ratio by global_ratio
                // Example: we want to measure FTV1 on 30% of total requests, but we the Apollo tracer samples at 50%.
                // If we measure FTV1 on 60% (0.3 / 0.5) of these sampled requests, that amounts to 30% of the total traffic
                (
                    SamplerOption::TraceIdRatioBased(global_ratio),
                    SamplerOption::TraceIdRatioBased(field_ratio),
                ) => field_ratio / global_ratio,
                (
                    SamplerOption::Always(Sampler::AlwaysOn),
                    SamplerOption::TraceIdRatioBased(field_ratio),
                ) => *field_ratio,
                (_, _) => 0.0,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_attribute_value_from_json() {
        assert_eq!(
            AttributeValue::try_from(json!("foo")),
            Ok(AttributeValue::String("foo".to_string()))
        );
        assert_eq!(
            AttributeValue::try_from(json!(1)),
            Ok(AttributeValue::I64(1))
        );
        assert_eq!(
            AttributeValue::try_from(json!(1.1)),
            Ok(AttributeValue::F64(1.1))
        );
        assert_eq!(
            AttributeValue::try_from(json!(true)),
            Ok(AttributeValue::Bool(true))
        );
        assert_eq!(
            AttributeValue::try_from(json!(["foo", "bar"])),
            Ok(AttributeValue::Array(AttributeArray::String(vec![
                "foo".to_string(),
                "bar".to_string()
            ])))
        );
        assert_eq!(
            AttributeValue::try_from(json!([1, 2])),
            Ok(AttributeValue::Array(AttributeArray::I64(vec![1, 2])))
        );
        assert_eq!(
            AttributeValue::try_from(json!([1.1, 1.5])),
            Ok(AttributeValue::Array(AttributeArray::F64(vec![1.1, 1.5])))
        );
        assert_eq!(
            AttributeValue::try_from(json!([true, false])),
            Ok(AttributeValue::Array(AttributeArray::Bool(vec![
                true, false
            ])))
        );

        // Mixed array conversions
        AttributeValue::try_from(json!(["foo", true])).expect_err("mixed conversion must fail");
        AttributeValue::try_from(json!([1, true])).expect_err("mixed conversion must fail");
        AttributeValue::try_from(json!([1.1, true])).expect_err("mixed conversion must fail");
        AttributeValue::try_from(json!([true, "bar"])).expect_err("mixed conversion must fail");
    }
}
