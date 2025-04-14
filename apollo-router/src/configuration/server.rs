use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

const DEFAULT_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

fn default_header_read_timeout() -> Duration {
    DEFAULT_HEADER_READ_TIMEOUT
}

/// Configuration for HTTP
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ServerHttpConfig {
    /// Header read timeout in human-readable format; defaults to 10s
    #[serde(
        deserialize_with = "humantime_serde::deserialize",
        default = "default_header_read_timeout"
    )]
    #[schemars(with = "String", default = "default_header_read_timeout")]
    pub(crate) header_read_timeout: Duration,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Server {
    /// The server http configuration
    pub(crate) http: ServerHttpConfig,
}

impl Default for ServerHttpConfig {
    fn default() -> Self {
        Self {
            header_read_timeout: Duration::from_secs(10),
        }
    }
}

#[buildstructor::buildstructor]
impl Server {
    #[builder]
    pub(crate) fn new(http: Option<ServerHttpConfig>) -> Self {
        Self {
            http: http.unwrap_or_default(),
        }
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::builder().build()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn it_builds_default_server_configuration() {
        let default_duration_seconds = Duration::from_secs(10);
        let server_config = Server::builder().build();
        assert_eq!(
            server_config.http.header_read_timeout,
            default_duration_seconds
        );
    }

    #[test]
    fn it_json_parses_default_header_read_timeout_when_server_http_config_omitted() {
        let json_server = json!({});

        let config: Server = serde_json::from_value(json_server).unwrap();

        assert_eq!(config.http.header_read_timeout, Duration::from_secs(10));
    }

    #[test]
    fn it_json_parses_default_header_read_timeout_when_omitted() {
        let json_config = json!({
            "http": {}
        });

        let config: Server = serde_json::from_value(json_config).unwrap();

        assert_eq!(config.http.header_read_timeout, Duration::from_secs(10));
    }

    #[test]
    fn it_json_parses_specified_server_config_seconds_correctly() {
        let json_config = json!({
           "http": {
               "header_read_timeout": "30s"
           }
        });

        let config: Server = serde_json::from_value(json_config).unwrap();

        assert_eq!(config.http.header_read_timeout, Duration::from_secs(30));
    }

    #[test]
    fn it_json_parses_specified_server_config_minutes_correctly() {
        let json_config = json!({
            "http": {
                "header_read_timeout": "1m"
            }
        });

        let config: Server = serde_json::from_value(json_config).unwrap();

        assert_eq!(config.http.header_read_timeout, Duration::from_secs(60));
    }
}
