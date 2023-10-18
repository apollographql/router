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
    Aws,
    Bunyan,
    Gelf,
    Google,
    OpenTelemetry,
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
