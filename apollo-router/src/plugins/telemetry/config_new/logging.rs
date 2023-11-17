use std::collections::BTreeMap;

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
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
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
    Json,

    /// https://docs.rs/tracing-subscriber/latest/tracing_subscriber/fmt/format/struct.Full.html
    #[default]
    Text,
}

/// The period to rollover the log file.
#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Rollover {
    /// Roll over every minute.
    Minutely,
    /// Roll over every hour.
    Hourly,
    /// Roll over every day.
    #[default]
    Daily,
    /// Never roll over.
    Never,
}
