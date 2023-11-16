use std::collections::BTreeMap;
use std::io::IsTerminal;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::telemetry::config::AttributeValue;

/// Logging configuration.
#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Logging {
    /// Common configuration
    pub(crate) common: LoggingCommon,
    /// Settings for logging to stdout.
    pub(crate) stdout: StdOut,
    /// Settings for logging to a file.
    pub(crate) file: File,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct LoggingCommon {
    /// Set a service.name resource in your metrics
    pub(crate) service_name: Option<String>,
    /// Set a service.namespace attribute in your metrics
    pub(crate) service_namespace: Option<String>,
    /// The Open Telemetry resource
    pub(crate) resource: BTreeMap<String, AttributeValue>,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct StdOut {
    /// Set to true to log to stdout.
    pub(crate) enabled: bool,
    /// The format to log to stdout.
    pub(crate) format: Format,
}

/// Log to a file
#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct File {
    /// Set to true to log to a file.
    pub(crate) enabled: bool,
    /// The path pattern of the file to log to.
    pub(crate) path: String,
    /// The format of the log file.
    pub(crate) format: Format,
    /// The period to rollover the log file.
    pub(crate) rollover: Rollover,
}

/// The format for logging.
#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Format {
    /// https://docs.aws.amazon.com/AmazonCloudWatch/latest/logs/CWL_AnalyzeLogData-discoverable-fields.html
    Aws,
    /// https://github.com/trentm/node-bunyan
    Bunyan,
    /// https://go2docs.graylog.org/5-0/getting_in_log_data/ingest_gelf.html#:~:text=The%20Graylog%20Extended%20Log%20Format,UDP%2C%20TCP%2C%20or%20HTTP.
    Gelf,

    /// https://cloud.google.com/logging/docs/structured-logging
    Google,
    /// https://github.com/open-telemetry/opentelemetry-rust/tree/main/opentelemetry-appender-log
    OpenTelemetry,

    /// https://docs.rs/tracing-subscriber/latest/tracing_subscriber/fmt/format/struct.Json.html
    #[serde(rename = "json")]
    JsonDefault,
    /// https://docs.rs/tracing-subscriber/latest/tracing_subscriber/fmt/format/struct.Json.html
    Json(JsonFormat),

    /// https://docs.rs/tracing-subscriber/latest/tracing_subscriber/fmt/format/struct.Full.html
    #[serde(rename = "text")]
    TextDefault,
    /// https://docs.rs/tracing-subscriber/latest/tracing_subscriber/fmt/format/struct.Full.html
    Text(TextFormat),
}

impl Default for Format {
    fn default() -> Self {
        if std::io::stdout().is_terminal() {
            Format::TextDefault
        } else {
            Format::JsonDefault
        }
    }
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case", default)]
pub(crate) struct JsonFormat {
    /// Move all span attributes to the top level json object.
    flatten_event: bool,
    /// Use ansi escape codes.
    ansi: bool,
    /// Include the timestamp with the log event.
    display_timestamp: bool,
    /// Include the target with the log event.
    display_target: bool,
    /// Include the level with the log event.
    display_level: bool,
    /// Include the thread_id with the log event.
    display_thread_id: bool,
    /// Include the thread_name with the log event.
    display_thread_name: bool,
    /// Include the filename with the log event.
    display_filename: bool,
    /// Include the line number with the log event.
    display_line_number: bool,
    /// Include the current span in this log event.
    display_current_span: bool,
    /// Include all of the containing span information with the log event.
    display_span_list: bool,
}

impl Default for JsonFormat {
    fn default() -> Self {
        JsonFormat {
            flatten_event: false,
            ansi: false,
            display_timestamp: true,
            display_target: true,
            display_level: true,
            display_thread_id: false,
            display_thread_name: false,
            display_filename: false,
            display_line_number: false,
            display_current_span: false,
            display_span_list: true,
        }
    }
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case", default)]
pub(crate) struct TextFormat {
    /// The type of text output, one of `default`, `compact`, or `full`.
    flavor: TextFlavor,
    /// Use ansi escape codes.
    ansi: bool,
    /// Include the timestamp with the log event.
    display_timestamp: bool,
    /// Include the target with the log event.
    display_target: bool,
    /// Include the level with the log event.
    display_level: bool,
    /// Include the thread_id with the log event.
    display_thread_id: bool,
    /// Include the thread_name with the log event.
    display_thread_name: bool,
    /// Include the filename with the log event.
    display_filename: bool,
    /// Include the line number with the log event.
    display_line_number: bool,
    /// Include the location with the log event.
    display_location: bool,
}

impl Default for TextFormat {
    fn default() -> Self {
        TextFormat {
            flavor: TextFlavor::Default,
            ansi: false,
            display_timestamp: true,
            display_target: false,
            display_level: true,
            display_thread_id: false,
            display_thread_name: false,
            display_filename: false,
            display_line_number: false,
            display_location: false,
        }
    }
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum TextFlavor {
    #[default]
    Default,
    Compact,
    Full,
}

/// The period to rollover the log file.
#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Rollover {
    /// Roll over every hour.
    Hourly,
    /// Roll over every day.
    Daily,
    #[default]
    /// Never roll over.
    Never,
}
