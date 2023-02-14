//! Configuration for the telemetry plugin.
use std::collections::BTreeMap;

use axum::headers::HeaderName;
use opentelemetry::sdk::resource::EnvResourceDetector;
use opentelemetry::sdk::resource::ResourceDetector;
use opentelemetry::sdk::trace::SpanLimits;
use opentelemetry::sdk::Resource;
use opentelemetry::Array;
use opentelemetry::KeyValue;
use opentelemetry::Value;
use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;

use super::metrics::MetricsAttributesConf;
use super::*;
use crate::configuration::ConfigurationError;
use crate::plugin::serde::deserialize_option_header_name;
use crate::plugin::serde::deserialize_regex;
use crate::plugins::telemetry::metrics;

#[derive(thiserror::Error, Debug)]
pub(crate) enum Error {
    #[error("field level instrumentation sampler must sample less frequently than tracing level sampler")]
    InvalidFieldLevelInstrumentationSampler,
}

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

/// Telemetry configuration
#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct Conf {
    /// Logging configuration
    #[serde(rename = "experimental_logging", default)]
    pub(crate) logging: Logging,
    /// Metrics configuration
    pub(crate) metrics: Option<Metrics>,
    /// Tracing configuration
    pub(crate) tracing: Option<Tracing>,
    /// Apollo reporting configuration
    pub(crate) apollo: Option<apollo::Config>,
}

/// Metrics configuration
#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) struct Metrics {
    /// Common metrics configuration across all exporters
    pub(crate) common: Option<MetricsCommon>,
    /// Open Telemetry native exporter configuration
    pub(crate) otlp: Option<otlp::Config>,
    /// Prometheus exporter configuration
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

/// Tracing configuration
#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) struct Tracing {
    // TODO: when deleting the `experimental_` prefix, check the usage when enabling dev mode
    // When deleting, put a #[serde(alias = "experimental_response_trace_id")] if we don't want to break things
    /// A way to expose trace id in response headers
    #[serde(default, rename = "experimental_response_trace_id")]
    pub(crate) response_trace_id: ExposeTraceId,
    /// Propagation configuration
    pub(crate) propagation: Option<Propagation>,
    /// Common configuration
    pub(crate) trace_config: Option<Trace>,
    /// OpenTelemetry native exporter configuration
    pub(crate) otlp: Option<otlp::Config>,
    /// Jaeger exporter configuration
    pub(crate) jaeger: Option<tracing::jaeger::Config>,
    /// Zipkin exporter configuration
    pub(crate) zipkin: Option<tracing::zipkin::Config>,
    /// Datadog exporter configuration
    pub(crate) datadog: Option<tracing::datadog::Config>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Logging {
    /// Log format
    pub(crate) format: LoggingFormat,
    /// Display the target in the logs
    pub(crate) display_target: bool,
    /// Display the filename in the logs
    pub(crate) display_filename: bool,
    /// Display the line number in the logs
    pub(crate) display_line_number: bool,
    /// Log configuration to log request and response for subgraphs and supergraph
    pub(crate) when_header: Vec<HeaderLoggingCondition>,
}

impl Logging {
    pub(crate) fn validate(&self) -> Result<(), ConfigurationError> {
        let misconfiguration = self.when_header.iter().any(|cfg| match cfg {
            HeaderLoggingCondition::Matching { headers, body, .. }
            | HeaderLoggingCondition::Value { headers, body, .. } => !body && !headers,
        });

        if misconfiguration {
            Err(ConfigurationError::InvalidConfiguration {
                message: "'when_header' configuration for logging is invalid",
                error: String::from(
                    "body and headers must not be both false because it doesn't enable any logs",
                ),
            })
        } else {
            Ok(())
        }
    }

    /// Returns if we should display the request/response headers and body given the `SupergraphRequest`
    pub(crate) fn should_log(&self, req: &SupergraphRequest) -> (bool, bool) {
        self.when_header
            .iter()
            .fold((false, false), |(log_headers, log_body), current| {
                let (current_log_headers, current_log_body) = current.should_log(req);
                (
                    log_headers || current_log_headers,
                    log_body || current_log_body,
                )
            })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum HeaderLoggingCondition {
    /// Match header value given a regex to display logs
    Matching {
        /// Header name
        name: String,
        /// Regex to match the header value
        #[schemars(with = "String", rename = "match")]
        #[serde(deserialize_with = "deserialize_regex", rename = "match")]
        matching: Regex,
        /// Display request/response headers (default: false)
        #[serde(default)]
        headers: bool,
        /// Display request/response body (default: false)
        #[serde(default)]
        body: bool,
    },
    /// Match header value given a value to display logs
    Value {
        /// Header name
        name: String,
        /// Header value
        value: String,
        /// Display request/response headers (default: false)
        #[serde(default)]
        headers: bool,
        /// Display request/response body (default: false)
        #[serde(default)]
        body: bool,
    },
}

impl HeaderLoggingCondition {
    /// Returns if we should display the request/response headers and body given the `SupergraphRequest`
    pub(crate) fn should_log(&self, req: &SupergraphRequest) -> (bool, bool) {
        match self {
            HeaderLoggingCondition::Matching {
                name,
                matching: matched,
                headers,
                body,
            } => {
                let header_match = req
                    .supergraph_request
                    .headers()
                    .get(name)
                    .and_then(|h| h.to_str().ok())
                    .map(|h| matched.is_match(h))
                    .unwrap_or_default();

                if header_match {
                    (*headers, *body)
                } else {
                    (false, false)
                }
            }
            HeaderLoggingCondition::Value {
                name,
                value,
                headers,
                body,
            } => {
                let header_match = req
                    .supergraph_request
                    .headers()
                    .get(name)
                    .and_then(|h| h.to_str().ok())
                    .map(|h| value.as_str() == h)
                    .unwrap_or_default();

                if header_match {
                    (*headers, *body)
                } else {
                    (false, false)
                }
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Copy)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum LoggingFormat {
    /// Pretty text format (default if you're running from a tty)
    Pretty,
    /// Json log format
    Json,
}

impl Default for LoggingFormat {
    fn default() -> Self {
        if atty::is(atty::Stream::Stdout) {
            Self::Pretty
        } else {
            Self::Json
        }
    }
}

#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case", default)]
pub(crate) struct ExposeTraceId {
    /// Expose the trace_id in response headers
    pub(crate) enabled: bool,
    /// Choose the header name to expose trace_id (default: apollo-trace-id)
    #[schemars(with = "Option<String>")]
    #[serde(deserialize_with = "deserialize_option_header_name")]
    pub(crate) header_name: Option<HeaderName>,
}

/// Configure propagation of traces. In general you won't have to do this as these are automatically configured
/// along with any exporter you configure.
#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case", default)]
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
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) struct RequestPropagation {
    /// Choose the header name to expose trace_id (default: apollo-trace-id)
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_option_header_name")]
    pub(crate) header_name: Option<HeaderName>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub(crate) struct Trace {
    /// The trace service name
    pub(crate) service_name: String,
    /// The trace service namespace
    pub(crate) service_namespace: String,
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
    /// Default attributes
    pub(crate) attributes: BTreeMap<String, AttributeValue>,
}

fn default_parent_based_sampler() -> bool {
    true
}

fn default_sampler() -> SamplerOption {
    SamplerOption::Always(Sampler::AlwaysOn)
}

impl Default for Trace {
    fn default() -> Self {
        Self {
            service_name: default_service_name(),
            service_namespace: default_service_namespace(),
            sampler: default_sampler(),
            parent_based_sampler: default_parent_based_sampler(),
            max_events_per_span: default_max_events_per_span(),
            max_attributes_per_span: default_max_attributes_per_span(),
            max_links_per_span: default_max_links_per_span(),
            max_attributes_per_event: default_max_attributes_per_event(),
            max_attributes_per_link: default_max_attributes_per_link(),
            attributes: Default::default(),
        }
    }
}

fn default_service_name() -> String {
    "${env.OTEL_SERVICE_NAME:-router}".to_string()
}
fn default_service_namespace() -> String {
    "".to_string()
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
            AttributeValue::String(v) => Value::String(v.into()),
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
    String(Vec<String>),
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

impl From<&Trace> for opentelemetry::sdk::trace::Config {
    fn from(config: &Trace) -> Self {
        let mut trace_config = opentelemetry::sdk::trace::config();

        let mut sampler: opentelemetry::sdk::trace::Sampler = config.sampler.clone().into();
        if config.parent_based_sampler {
            sampler = parent_based(sampler);
        }

        trace_config = trace_config.with_sampler(sampler);
        trace_config = trace_config.with_max_events_per_span(config.max_events_per_span);
        trace_config = trace_config.with_max_attributes_per_span(config.max_attributes_per_span);
        trace_config = trace_config.with_max_links_per_span(config.max_links_per_span);
        trace_config = trace_config.with_max_attributes_per_event(config.max_attributes_per_event);
        trace_config = trace_config.with_max_attributes_per_link(config.max_attributes_per_link);

        let mut resource_defaults = vec![];
        resource_defaults.push(KeyValue::new(
            opentelemetry_semantic_conventions::resource::SERVICE_NAME,
            config.service_name.clone(),
        ));
        resource_defaults.push(KeyValue::new(
            opentelemetry_semantic_conventions::resource::SERVICE_NAMESPACE,
            config.service_namespace.clone(),
        ));
        resource_defaults.push(KeyValue::new(
            opentelemetry_semantic_conventions::resource::SERVICE_VERSION,
            std::env!("CARGO_PKG_VERSION"),
        ));

        if let Some(executable_name) = std::env::current_exe().ok().and_then(|path| {
            path.file_name()
                .and_then(|p| p.to_str().map(|s| s.to_string()))
        }) {
            resource_defaults.push(KeyValue::new(
                opentelemetry_semantic_conventions::resource::PROCESS_EXECUTABLE_NAME,
                executable_name,
            ));
        }

        // Take the env variables first, and then layer on the rest of the resources, last entry wins
        let resource = EnvResourceDetector::default()
            .detect(Duration::from_secs(0))
            .merge(&Resource::new(resource_defaults))
            .merge(&mut Resource::new(
                config
                    .attributes
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

impl Conf {
    pub(crate) fn calculate_field_level_instrumentation_ratio(&self) -> Result<f64, Error> {
        Ok(
            match (
                self.tracing
                    .clone()
                    .unwrap_or_default()
                    .trace_config
                    .unwrap_or_default()
                    .sampler,
                self.apollo
                    .clone()
                    .unwrap_or_default()
                    .field_level_instrumentation_sampler,
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
                ) if ratio != 0.0 => Err(Error::InvalidFieldLevelInstrumentationSampler)?,
                (
                    SamplerOption::TraceIdRatioBased(ratio),
                    SamplerOption::Always(Sampler::AlwaysOn),
                ) if ratio != 1.0 => Err(Error::InvalidFieldLevelInstrumentationSampler)?,

                // Happy paths
                (_, SamplerOption::TraceIdRatioBased(ratio)) if ratio == 0.0 => 0.0,
                (SamplerOption::TraceIdRatioBased(ratio), _) if ratio == 0.0 => 0.0,
                (_, SamplerOption::Always(Sampler::AlwaysOn)) => 1.0,
                (
                    SamplerOption::TraceIdRatioBased(global_ratio),
                    SamplerOption::TraceIdRatioBased(field_ratio),
                ) => field_ratio / global_ratio,
                (
                    SamplerOption::Always(Sampler::AlwaysOn),
                    SamplerOption::TraceIdRatioBased(field_ratio),
                ) => field_ratio,
                (_, _) => 0.0,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logging_conf_validation() {
        let logging_conf = Logging {
            format: LoggingFormat::default(),
            display_target: false,
            display_filename: false,
            display_line_number: false,
            when_header: vec![HeaderLoggingCondition::Value {
                name: "test".to_string(),
                value: String::new(),
                headers: true,
                body: false,
            }],
        };

        logging_conf.validate().unwrap();

        let logging_conf = Logging {
            format: LoggingFormat::default(),
            display_target: false,
            display_filename: false,
            display_line_number: false,
            when_header: vec![HeaderLoggingCondition::Value {
                name: "test".to_string(),
                value: String::new(),
                headers: false,
                body: false,
            }],
        };

        let validate_res = logging_conf.validate();
        assert!(validate_res.is_err());
        assert_eq!(validate_res.unwrap_err().to_string(), "'when_header' configuration for logging is invalid: body and headers must not be both false because it doesn't enable any logs");
    }

    #[test]
    fn test_logging_conf_should_log() {
        let logging_conf = Logging {
            format: LoggingFormat::default(),
            display_target: false,
            display_filename: false,
            display_line_number: false,
            when_header: vec![HeaderLoggingCondition::Matching {
                name: "test".to_string(),
                matching: Regex::new("^foo*").unwrap(),
                headers: true,
                body: false,
            }],
        };
        let req = SupergraphRequest::fake_builder()
            .header("test", "foobar")
            .build()
            .unwrap();
        assert_eq!(logging_conf.should_log(&req), (true, false));

        let logging_conf = Logging {
            format: LoggingFormat::default(),
            display_target: false,
            display_filename: false,
            display_line_number: false,
            when_header: vec![HeaderLoggingCondition::Value {
                name: "test".to_string(),
                value: String::from("foobar"),
                headers: true,
                body: false,
            }],
        };
        assert_eq!(logging_conf.should_log(&req), (true, false));

        let logging_conf = Logging {
            format: LoggingFormat::default(),
            display_target: false,
            display_filename: false,
            display_line_number: false,
            when_header: vec![
                HeaderLoggingCondition::Matching {
                    name: "test".to_string(),
                    matching: Regex::new("^foo*").unwrap(),
                    headers: true,
                    body: false,
                },
                HeaderLoggingCondition::Matching {
                    name: "test".to_string(),
                    matching: Regex::new("^*bar$").unwrap(),
                    headers: false,
                    body: true,
                },
            ],
        };
        assert_eq!(logging_conf.should_log(&req), (true, true));

        let logging_conf = Logging {
            format: LoggingFormat::default(),
            display_target: false,
            display_filename: false,
            display_line_number: false,
            when_header: vec![HeaderLoggingCondition::Matching {
                name: "testtest".to_string(),
                matching: Regex::new("^foo*").unwrap(),
                headers: true,
                body: false,
            }],
        };
        assert_eq!(logging_conf.should_log(&req), (false, false));
    }
}
