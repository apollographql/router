//! Configuration for the telemetry plugin.
use std::borrow::Cow;
use std::collections::BTreeMap;

use opentelemetry::sdk::Resource;
use opentelemetry::Array;
use opentelemetry::KeyValue;
use opentelemetry::Value;
use schemars::JsonSchema;
use serde::Deserialize;

use super::metrics::MetricsAttributesConf;
use super::*;
use crate::plugins::telemetry::metrics;

pub(crate) trait GenericWith<T>
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

#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct Conf {
    #[allow(dead_code)]
    pub(crate) metrics: Option<Metrics>,
    pub(crate) tracing: Option<Tracing>,
    pub(crate) apollo: Option<apollo::Config>,
}

#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) struct Metrics {
    pub(crate) common: Option<MetricsCommon>,
    pub(crate) otlp: Option<otlp::Config>,
    pub(crate) prometheus: Option<metrics::prometheus::Config>,
}

#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) struct MetricsCommon {
    /// Configuration to add custom labels/attributes to metrics
    pub(crate) attributes: Option<MetricsAttributesConf>,
    /// Set a service.name resource in your metrics
    pub(crate) service_name: Option<String>,
    /// Set a service.namespace attribute in your metrics
    pub(crate) service_namespace: Option<String>,
    #[serde(default)]
    /// Resources
    pub(crate) resources: HashMap<String, String>,
}

#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) struct Tracing {
    pub(crate) propagation: Option<Propagation>,
    pub(crate) trace_config: Option<Trace>,
    pub(crate) otlp: Option<otlp::Config>,
    pub(crate) jaeger: Option<tracing::jaeger::Config>,
    pub(crate) zipkin: Option<tracing::zipkin::Config>,
    pub(crate) datadog: Option<tracing::datadog::Config>,
}

#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) struct Propagation {
    pub(crate) baggage: Option<bool>,
    pub(crate) trace_context: Option<bool>,
    pub(crate) jaeger: Option<bool>,
    pub(crate) datadog: Option<bool>,
    pub(crate) zipkin: Option<bool>,
}

#[derive(Default, Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub(crate) struct Trace {
    pub(crate) service_name: Option<String>,
    pub(crate) service_namespace: Option<String>,
    pub(crate) sampler: Option<SamplerOption>,
    pub(crate) parent_based_sampler: Option<bool>,
    pub(crate) max_events_per_span: Option<u32>,
    pub(crate) max_attributes_per_span: Option<u32>,
    pub(crate) max_links_per_span: Option<u32>,
    pub(crate) max_attributes_per_event: Option<u32>,
    pub(crate) max_attributes_per_link: Option<u32>,
    pub(crate) attributes: Option<BTreeMap<String, AttributeValue>>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
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

impl From<AttributeValue> for opentelemetry::Value {
    fn from(value: AttributeValue) -> Self {
        match value {
            AttributeValue::Bool(v) => Value::Bool(v),
            AttributeValue::I64(v) => Value::I64(v),
            AttributeValue::F64(v) => Value::F64(v),
            AttributeValue::String(v) => Value::String(Cow::from(v)),
            AttributeValue::Array(v) => Value::Array(v.into()),
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub(crate) enum AttributeArray {
    /// Array of bools
    Bool(Vec<bool>),
    /// Array of integers
    I64(Vec<i64>),
    /// Array of floats
    F64(Vec<f64>),
    /// Array of strings
    String(Vec<Cow<'static, str>>),
}

impl From<AttributeArray> for opentelemetry::Array {
    fn from(array: AttributeArray) -> Self {
        match array {
            AttributeArray::Bool(v) => Array::Bool(v),
            AttributeArray::I64(v) => Array::I64(v),
            AttributeArray::F64(v) => Array::F64(v),
            AttributeArray::String(v) => Array::String(v),
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum SamplerOption {
    /// Sample a given fraction of traces. Fractions >= 1 will always sample. If the parent span is
    /// sampled, then it's child spans will automatically be sampled. Fractions < 0 are treated as
    /// zero, but spans may still be sampled if their parent is.
    TraceIdRatioBased(f64),
    Always(Sampler),
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Sampler {
    /// Always sample the trace
    AlwaysOn,
    /// Never sample the trace
    AlwaysOff,
}

impl From<&Trace> for opentelemetry::sdk::trace::Config {
    fn from(config: &Trace) -> Self {
        let mut trace_config = opentelemetry::sdk::trace::config();

        let sampler = match (&config.sampler, &config.parent_based_sampler) {
            (Some(SamplerOption::Always(Sampler::AlwaysOn)), Some(true)) => {
                Some(parent_based(opentelemetry::sdk::trace::Sampler::AlwaysOn))
            }
            (Some(SamplerOption::Always(Sampler::AlwaysOff)), Some(true)) => {
                Some(parent_based(opentelemetry::sdk::trace::Sampler::AlwaysOff))
            }
            (Some(SamplerOption::TraceIdRatioBased(ratio)), Some(true)) => Some(parent_based(
                opentelemetry::sdk::trace::Sampler::TraceIdRatioBased(*ratio),
            )),
            (Some(SamplerOption::Always(Sampler::AlwaysOn)), _) => {
                Some(opentelemetry::sdk::trace::Sampler::AlwaysOn)
            }
            (Some(SamplerOption::Always(Sampler::AlwaysOff)), _) => {
                Some(opentelemetry::sdk::trace::Sampler::AlwaysOff)
            }
            (Some(SamplerOption::TraceIdRatioBased(ratio)), _) => Some(
                opentelemetry::sdk::trace::Sampler::TraceIdRatioBased(*ratio),
            ),
            (_, _) => None,
        };
        if let Some(sampler) = sampler {
            trace_config = trace_config.with_sampler(sampler);
        }
        if let Some(n) = config.max_events_per_span {
            trace_config = trace_config.with_max_events_per_span(n);
        }
        if let Some(n) = config.max_attributes_per_span {
            trace_config = trace_config.with_max_attributes_per_span(n);
        }
        if let Some(n) = config.max_links_per_span {
            trace_config = trace_config.with_max_links_per_span(n);
        }
        if let Some(n) = config.max_attributes_per_event {
            trace_config = trace_config.with_max_attributes_per_event(n);
        }
        if let Some(n) = config.max_attributes_per_link {
            trace_config = trace_config.with_max_attributes_per_link(n);
        }

        let mut resource_defaults = vec![];
        if let Some(service_name) = &config.service_name {
            resource_defaults.push(KeyValue::new(
                opentelemetry_semantic_conventions::resource::SERVICE_NAME,
                service_name.clone(),
            ));
        } else if std::env::var("OTEL_SERVICE_NAME").is_err() {
            resource_defaults.push(KeyValue::new(
                opentelemetry_semantic_conventions::resource::SERVICE_NAME,
                "router".to_string(),
            ));
        }
        if let Some(service_namespace) = &config.service_namespace {
            resource_defaults.push(KeyValue::new(
                opentelemetry_semantic_conventions::resource::SERVICE_NAMESPACE,
                service_namespace.clone(),
            ));
        }

        if let Some(executable_name) = std::env::current_exe().ok().and_then(|path| {
            path.file_name()
                .and_then(|p| p.to_str().map(|s| s.to_string()))
        }) {
            resource_defaults.push(KeyValue::new(
                opentelemetry_semantic_conventions::resource::PROCESS_EXECUTABLE_NAME,
                executable_name,
            ));
        }

        let resource = Resource::new(resource_defaults).merge(&mut Resource::new(
            config
                .attributes
                .clone()
                .unwrap_or_default()
                .iter()
                .map(|(k, v)| {
                    KeyValue::new(
                        opentelemetry::Key::from(k.clone()),
                        opentelemetry::Value::from(v.clone()),
                    )
                })
                .collect::<Vec<KeyValue>>(),
        ));

        trace_config = trace_config.with_resource(resource);
        trace_config
    }
}

fn parent_based(sampler: opentelemetry::sdk::trace::Sampler) -> opentelemetry::sdk::trace::Sampler {
    opentelemetry::sdk::trace::Sampler::ParentBased(Box::new(sampler))
}
