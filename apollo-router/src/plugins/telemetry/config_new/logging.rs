use schemars::JsonSchema;
use serde::Deserialize;

/// Logging configuration.
#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Logging {
    /// Settings for logging to stdout.
    stdout: StdOut,
    /// Settings for logging to a file.
    file: File,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
struct StdOut {
    /// Set to true to log to stdout.
    enabled: bool,
    /// The format to log to stdout.
    format: Format,
}

/// Log to a file
#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
struct File {
    /// Set to true to log to a file.
    enabled: bool,
    /// The path pattern of the file to log to.
    path: String,
    /// The format of the log file.
    format: Format,
    /// The period to rollover the log file.
    rollover: Rollover,
}

/// The format for logging.
#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
enum Format {
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
enum Rollover {
    /// Roll over every minute.
    Minutely,
    /// Roll over every hour.
    Hourly,
    /// Roll over every day.
    Daily,
    /// Never roll over.
    #[default]
    Never,
}
