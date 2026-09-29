use std::collections::BTreeMap;
use std::collections::HashSet;
use std::io::IsTerminal;
use std::time::Duration;

use schemars::JsonSchema;
use schemars::Schema;
use schemars::SchemaGenerator;
use serde::Deserialize;
use serde::Deserializer;
use serde::de::MapAccess;
use serde::de::Visitor;

use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config::TraceIdFormat;
use crate::plugins::telemetry::resource::ConfigResource;

/// Logging configuration.
#[apollo_configuration::configuration]
#[derive(PartialEq)]
pub(crate) struct Logging {
    /// Common configuration
    pub(crate) common: LoggingCommon,
    /// Settings for logging to stdout.
    pub(crate) stdout: StdOut,
    #[serde(skip)]
    /// Settings for logging to a file.
    pub(crate) file: File,
}

#[apollo_configuration::configuration]
#[derive(PartialEq)]
pub(crate) struct LoggingCommon {
    /// Set a service.name resource in your metrics
    pub(crate) service_name: Option<String>,
    /// Set a service.namespace attribute in your metrics
    pub(crate) service_namespace: Option<String>,
    /// The Open Telemetry resource
    // BTreeMap has no Validate impl.
    #[config(skip_validate)]
    pub(crate) resource: BTreeMap<String, AttributeValue>,
}

impl ConfigResource for LoggingCommon {
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

#[apollo_configuration::configuration]
#[derive(PartialEq)]
pub(crate) struct StdOut {
    /// Set to true to log to stdout.
    #[config(default = true)]
    pub(crate) enabled: bool,
    /// The format to log to stdout.
    pub(crate) format: Format,
    /// The format to log to stdout when you're running on an interactive terminal. When configured it will automatically use this `tty_format`` instead of the original `format` when an interactive terminal is detected
    pub(crate) tty_format: Option<Format>,
    /// Log rate limiting. The limit is set per type of log message
    pub(crate) rate_limit: RateLimit,
}

#[apollo_configuration::configuration]
#[derive(PartialEq)]
pub(crate) struct RateLimit {
    /// Set to true to limit the rate of log messages
    pub(crate) enabled: bool,
    /// Number of log lines allowed in interval per message
    #[config(default = 1)]
    pub(crate) capacity: u32,
    /// Interval for rate limiting
    #[config(default = Duration::from_secs(1).into())]
    #[schemars(with = "String")]
    pub(crate) interval: apollo_configuration::types::Duration,
}

/// Log to a file
#[apollo_configuration::configuration]
#[allow(dead_code)]
#[derive(PartialEq)]
pub(crate) struct File {
    /// Set to true to log to a file.
    pub(crate) enabled: bool,
    /// The path pattern of the file to log to.
    pub(crate) path: String,
    /// The format of the log file.
    pub(crate) format: Format,
    /// The period to rollover the log file.
    pub(crate) rollover: Rollover,
    /// Log rate limiting. The limit is set per type of log message
    pub(crate) rate_limit: Option<RateLimit>,
}

/// The format for logging.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Format {
    // !!!!WARNING!!!!, if you change this enum then be sure to add the changes to the JsonSchema AND the custom deserializer.

    // Want to see support for these formats? Please open an issue!
    // /// https://docs.aws.amazon.com/AmazonCloudWatch/latest/logs/CWL_AnalyzeLogData-discoverable-fields.html
    // Aws,
    // /// https://github.com/trentm/node-bunyan
    // Bunyan,
    // /// https://go2docs.graylog.org/5-0/getting_in_log_data/ingest_gelf.html#:~:text=The%20Graylog%20Extended%20Log%20Format,UDP%2C%20TCP%2C%20or%20HTTP.
    // Gelf,
    //
    // /// https://cloud.google.com/logging/docs/structured-logging
    // Google,
    // /// https://github.com/open-telemetry/opentelemetry-rust/tree/main/opentelemetry-appender-log
    // OpenTelemetry,
    /// https://docs.rs/tracing-subscriber/latest/tracing_subscriber/fmt/format/struct.Json.html
    Json(JsonFormat),

    /// https://docs.rs/tracing-subscriber/latest/tracing_subscriber/fmt/format/struct.Full.html
    Text(TextFormat),
}

// This custom implementation JsonSchema allows the user to supply an enum or a struct in the same way that the custom deserializer does.
impl JsonSchema for Format {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "logging_format".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let types = vec![
            (
                "json",
                JsonFormat::json_schema(generator),
                "Tracing subscriber https://docs.rs/tracing-subscriber/latest/tracing_subscriber/fmt/format/struct.Json.html",
            ),
            (
                "text",
                TextFormat::json_schema(generator),
                "Tracing subscriber https://docs.rs/tracing-subscriber/latest/tracing_subscriber/fmt/format/struct.Full.html",
            ),
        ];

        let schemas = types
            .into_iter()
            .flat_map(|(name, schema, description)| {
                [
                    schemars::json_schema!({
                        "type": "object",
                        "description": description,
                        "properties": {
                            name.to_string(): schema,
                        },
                        "required": [name],
                        "additionalProperties": false,
                    }),
                    schemars::json_schema!({
                        "type": "string",
                        "description": description,
                        "enum": [name], // TODO(@goto-bus-stop): why not "const"?
                    }),
                ]
            })
            .collect::<Vec<_>>();

        schemars::json_schema!({
            "oneOf": schemas,
        })
    }
}

// Format's hand-written Deserialize accepts a bare format name as well as a map, which the
// configuration attribute can't express, so this forwards validation to the selected format.
impl apollo_configuration::Validate for Format {
    fn validate(&self, mut errors: apollo_configuration::ErrorCollector<'_>) {
        match self {
            Format::Json(format) => format.validate(errors.nest("json")),
            Format::Text(format) => format.validate(errors.nest("text")),
        }
    }
}

impl<'de> Deserialize<'de> for Format {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StringOrStruct;

        impl<'de> Visitor<'de> for StringOrStruct {
            type Value = Format;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("string or enum")
            }

            fn visit_str<E>(self, value: &str) -> Result<Format, E>
            where
                E: serde::de::Error,
            {
                match value {
                    "json" => Ok(Format::Json(JsonFormat::default())),
                    "text" => Ok(Format::Text(TextFormat::default())),
                    _ => Err(E::custom(format!("unknown log format: {value}"))),
                }
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let key = map.next_key::<String>()?;

                match key.as_deref() {
                    Some("json") => Ok(Format::Json(map.next_value::<JsonFormat>()?)),
                    Some("text") => Ok(Format::Text(map.next_value::<TextFormat>()?)),
                    Some(value) => Err(serde::de::Error::custom(format!(
                        "unknown log format: {value}"
                    ))),
                    _ => Err(serde::de::Error::custom("unknown log format")),
                }
            }
        }

        deserializer.deserialize_any(StringOrStruct)
    }
}

impl Default for Format {
    fn default() -> Self {
        if std::io::stdout().is_terminal() {
            Format::Text(TextFormat::default())
        } else {
            Format::Json(JsonFormat::default())
        }
    }
}

#[apollo_configuration::configuration]
#[derive(Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) struct JsonFormat {
    /// Include the timestamp with the log event. (default: true)
    #[config(default = true)]
    pub(crate) display_timestamp: bool,
    /// Include the target with the log event. (default: true)
    #[config(default = true)]
    pub(crate) display_target: bool,
    /// Include the level with the log event. (default: true)
    #[config(default = true)]
    pub(crate) display_level: bool,
    /// Include the thread_id with the log event.
    pub(crate) display_thread_id: bool,
    /// Include the thread_name with the log event.
    pub(crate) display_thread_name: bool,
    /// Include the filename with the log event.
    pub(crate) display_filename: bool,
    /// Include the line number with the log event.
    pub(crate) display_line_number: bool,
    /// Include the current span in this log event.
    pub(crate) display_current_span: bool,
    /// Include all of the containing span information with the log event. (default: true)
    #[config(default = true)]
    pub(crate) display_span_list: bool,
    /// Include the resource with the log event. (default: true)
    #[config(default = true)]
    pub(crate) display_resource: bool,
    /// Include the trace id (if any) with the log event. (default: true)
    #[config(default = DisplayTraceIdFormat::Bool(true))]
    pub(crate) display_trace_id: DisplayTraceIdFormat,
    /// Include the span id (if any) with the log event. (default: true)
    #[config(default = true)]
    pub(crate) display_span_id: bool,
    /// List of span attributes to attach to the json log object
    // HashSet has no Validate impl.
    #[config(skip_validate)]
    pub(crate) span_attributes: HashSet<String>,
    /// Output string attribute values that contain valid JSON objects or arrays
    /// as native JSON rather than quoted strings. Useful for log aggregators
    /// (e.g. Splunk) that can index nested JSON fields. (default: false)
    pub(crate) expand_json_string_values: bool,
}

#[apollo_configuration::configuration]
#[derive(PartialEq, Eq)]
#[serde(untagged)]
pub(crate) enum DisplayTraceIdFormat {
    // /// Format the Trace ID as a hexadecimal number
    // ///
    // /// (e.g. Trace ID 16 -> 00000000000000000000000000000010)
    // #[default]
    // Hexadecimal,
    // /// Format the Trace ID as a hexadecimal number
    // ///
    // /// (e.g. Trace ID 16 -> 00000000000000000000000000000010)
    // OpenTelemetry,
    // /// Format the Trace ID as a decimal number
    // ///
    // /// (e.g. Trace ID 16 -> 16)
    // Decimal,

    // /// Datadog
    // Datadog,

    // /// UUID format with dashes
    // /// (eg. 67e55044-10b1-426f-9247-bb680e5fe0c8)
    // Uuid,
    #[config(default)]
    TraceIdFormat(TraceIdFormat),
    Bool(bool),
}

#[apollo_configuration::configuration]
#[derive(Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) struct TextFormat {
    /// Process ansi escapes (default: true)
    #[config(default = true)]
    pub(crate) ansi_escape_codes: bool,
    /// Include the timestamp with the log event. (default: true)
    #[config(default = true)]
    pub(crate) display_timestamp: bool,
    /// Include the target with the log event.
    pub(crate) display_target: bool,
    /// Include the level with the log event. (default: true)
    #[config(default = true)]
    pub(crate) display_level: bool,
    /// Include the thread_id with the log event.
    pub(crate) display_thread_id: bool,
    /// Include the thread_name with the log event.
    pub(crate) display_thread_name: bool,
    /// Include the filename with the log event.
    pub(crate) display_filename: bool,
    /// Include the line number with the log event.
    pub(crate) display_line_number: bool,
    /// Include the service namespace with the log event.
    pub(crate) display_service_namespace: bool,
    /// Include the service name with the log event.
    pub(crate) display_service_name: bool,
    /// Include the resource with the log event.
    pub(crate) display_resource: bool,
    /// Include the current span in this log event. (default: true)
    #[config(default = true)]
    pub(crate) display_current_span: bool,
    /// Include all of the containing span information with the log event. (default: true)
    #[config(default = true)]
    pub(crate) display_span_list: bool,
    /// Include the trace id (if any) with the log event. (default: false)
    #[config(default = DisplayTraceIdFormat::Bool(false))]
    pub(crate) display_trace_id: DisplayTraceIdFormat,
    /// Include the span id (if any) with the log event. (default: false)
    pub(crate) display_span_id: bool,
}

/// The period to rollover the log file.
#[apollo_configuration::configuration]
#[allow(dead_code)]
#[derive(PartialEq)]
pub(crate) enum Rollover {
    /// Roll over every hour.
    Hourly,
    /// Roll over every day.
    Daily,
    #[config(default)]
    /// Never roll over.
    Never,
}

#[cfg(test)]
mod test {
    use serde_json::json;

    use crate::plugins::telemetry::config_new::logging::Format;

    #[test]
    fn format_de() {
        let format = serde_json::from_value::<Format>(json!("text")).unwrap();
        assert_eq!(format, Format::Text(Default::default()));
        let format = serde_json::from_value::<Format>(json!("json")).unwrap();
        assert_eq!(format, Format::Json(Default::default()));
        let format = serde_json::from_value::<Format>(json!({"text":{}})).unwrap();
        assert_eq!(format, Format::Text(Default::default()));
        let format = serde_json::from_value::<Format>(json!({"json":{}})).unwrap();
        assert_eq!(format, Format::Json(Default::default()));
    }
}
