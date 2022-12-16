//! Configuration for the telemetry plugin.
use std::collections::BTreeMap;

use axum::headers::HeaderName;
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
use crate::plugin::serde::deserialize_header_name;
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

#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct Conf {
    #[serde(rename = "experimental_logging")]
    pub(crate) logging: Option<Logging>,
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
    // TODO: when deleting the `experimental_` prefix, check the usage when enabling dev mode
    // When deleting, put a #[serde(alias = "experimental_response_trace_id")] if we don't want to break things
    /// A way to expose trace id in response headers
    #[serde(default, rename = "experimental_response_trace_id")]
    pub(crate) response_trace_id: ExposeTraceId,
    pub(crate) propagation: Option<Propagation>,
    pub(crate) trace_config: Option<Trace>,
    pub(crate) otlp: Option<otlp::Config>,
    pub(crate) jaeger: Option<tracing::jaeger::Config>,
    pub(crate) zipkin: Option<tracing::zipkin::Config>,
    pub(crate) datadog: Option<tracing::datadog::Config>,
}

#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Logging {
    /// Log format
    #[serde(default)]
    pub(crate) format: LoggingFormat,
    #[serde(default = "default_display_filename")]
    pub(crate) display_filename: bool,
    #[serde(default = "default_display_line_number")]
    pub(crate) display_line_number: bool,
    /// Log configuration to log request and response for subgraphs and supergraph
    #[serde(default)]
    pub(crate) when_header: Vec<HeaderLoggingCondition>,
}

pub(crate) const fn default_display_filename() -> bool {
    true
}

pub(crate) const fn default_display_line_number() -> bool {
    true
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
        #[schemars(schema_with = "string_schema", rename = "match")]
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
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) struct ExposeTraceId {
    /// Expose the trace_id in response headers
    pub(crate) enabled: bool,
    /// Choose the header name to expose trace_id (default: apollo-trace-id)
    #[schemars(with = "Option<String>")]
    #[serde(deserialize_with = "deserialize_option_header_name")]
    pub(crate) header_name: Option<HeaderName>,
}

#[derive(Clone, Default, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) struct Propagation {
    /// Select a custom request header to set your own trace_id (header value must be convertible from hexadecimal to set a correct trace_id)
    pub(crate) request: Option<PropagationRequestTraceId>,
    pub(crate) baggage: Option<bool>,
    pub(crate) trace_context: Option<bool>,
    pub(crate) jaeger: Option<bool>,
    pub(crate) datadog: Option<bool>,
    pub(crate) zipkin: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) struct PropagationRequestTraceId {
    /// Choose the header name to expose trace_id (default: apollo-trace-id)
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_header_name")]
    pub(crate) header_name: HeaderName,
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
                    Some(SamplerOption::TraceIdRatioBased(global_ratio)),
                    Some(SamplerOption::TraceIdRatioBased(field_ratio)),
                ) if field_ratio > global_ratio => {
                    Err(Error::InvalidFieldLevelInstrumentationSampler)?
                }
                (
                    Some(SamplerOption::Always(Sampler::AlwaysOff)),
                    Some(SamplerOption::Always(Sampler::AlwaysOn)),
                ) => Err(Error::InvalidFieldLevelInstrumentationSampler)?,
                (
                    Some(SamplerOption::Always(Sampler::AlwaysOff)),
                    Some(SamplerOption::TraceIdRatioBased(ratio)),
                ) if ratio != 0.0 => Err(Error::InvalidFieldLevelInstrumentationSampler)?,
                (
                    Some(SamplerOption::TraceIdRatioBased(ratio)),
                    Some(SamplerOption::Always(Sampler::AlwaysOn)),
                ) if ratio != 1.0 => Err(Error::InvalidFieldLevelInstrumentationSampler)?,

                // Happy paths
                (_, Some(SamplerOption::TraceIdRatioBased(ratio))) if ratio == 0.0 => 0.0,
                (Some(SamplerOption::TraceIdRatioBased(ratio)), _) if ratio == 0.0 => 0.0,
                (_, Some(SamplerOption::Always(Sampler::AlwaysOn))) => 1.0,
                (
                    Some(SamplerOption::TraceIdRatioBased(global_ratio)),
                    Some(SamplerOption::TraceIdRatioBased(field_ratio)),
                ) => field_ratio / global_ratio,
                (
                    Some(SamplerOption::Always(Sampler::AlwaysOn)),
                    Some(SamplerOption::TraceIdRatioBased(field_ratio)),
                ) => field_ratio,
                (_, _) => 0.0,
            },
        )
    }
}

fn string_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    String::json_schema(gen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logging_conf_validation() {
        let logging_conf = Logging {
            format: LoggingFormat::default(),
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
