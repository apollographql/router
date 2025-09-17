use std::time::Duration;

use bytesize::ByteSize;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

const DEFAULT_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

fn default_header_read_timeout() -> Duration {
    DEFAULT_HEADER_READ_TIMEOUT
}

/// Configuration for HTTP limits
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ServerHttpMaxConfig {
    /// Maximum size of a single header field (name + value) in bytes.
    /// Applies to both HTTP/1.1 and HTTP/2.
    /// If not specified, uses the underlying HTTP implementation's default.
    #[schemars(with = "Option<String>")]
    pub(crate) header_size: Option<ByteSize>,

    /// Maximum number of headers allowed in a request.
    /// Applies primarily to HTTP/1.1 connections.
    /// If not specified, uses the underlying HTTP implementation's default.
    pub(crate) headers: Option<usize>,

    /// Maximum total size of all headers combined in bytes.
    /// Applies primarily to HTTP/2 connections.
    /// If not specified, uses the underlying HTTP implementation's default.
    #[schemars(with = "Option<String>")]
    pub(crate) header_list_size: Option<ByteSize>,
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

    /// Maximum limits for HTTP requests
    pub(crate) max: ServerHttpMaxConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Server {
    /// The server http configuration
    pub(crate) http: ServerHttpConfig,
}

impl Default for ServerHttpMaxConfig {
    fn default() -> Self {
        Self {
            header_size: None,
            headers: None,
            header_list_size: None,
        }
    }
}

impl Default for ServerHttpConfig {
    fn default() -> Self {
        Self {
            header_read_timeout: Duration::from_secs(10),
            max: ServerHttpMaxConfig::default(),
        }
    }
}

#[buildstructor::buildstructor]
impl ServerHttpMaxConfig {
    #[builder]
    pub(crate) fn new(
        header_size: Option<ByteSize>,
        headers: Option<usize>,
        header_list_size: Option<ByteSize>,
    ) -> Self {
        Self {
            header_size,
            headers,
            header_list_size,
        }
    }
}

#[buildstructor::buildstructor]
impl ServerHttpConfig {
    #[builder]
    pub(crate) fn new(
        header_read_timeout: Option<Duration>,
        max: Option<ServerHttpMaxConfig>,
    ) -> Self {
        Self {
            header_read_timeout: header_read_timeout.unwrap_or_else(default_header_read_timeout),
            max: max.unwrap_or_default(),
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
        assert_eq!(server_config.http.max.header_size, None);
        assert_eq!(server_config.http.max.headers, None);
        assert_eq!(server_config.http.max.header_list_size, None);
    }

    #[test]
    fn it_json_parses_default_header_read_timeout_when_server_http_config_omitted() {
        let json_server = json!({});

        let config: Server = serde_json::from_value(json_server).unwrap();

        assert_eq!(config.http.header_read_timeout, Duration::from_secs(10));
        assert_eq!(config.http.max.header_size, None);
        assert_eq!(config.http.max.headers, None);
        assert_eq!(config.http.max.header_list_size, None);
    }

    #[test]
    fn it_json_parses_default_header_read_timeout_when_omitted() {
        let json_config = json!({
            "http": {}
        });

        let config: Server = serde_json::from_value(json_config).unwrap();

        assert_eq!(config.http.header_read_timeout, Duration::from_secs(10));
        assert_eq!(config.http.max.header_size, None);
        assert_eq!(config.http.max.headers, None);
        assert_eq!(config.http.max.header_list_size, None);
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

    #[test]
    fn it_json_parses_http_header_limits_correctly() {
        let json_config = json!({
            "http": {
                "max": {
                    "header_size": "32kb",
                    "headers": 200,
                    "header_list_size": "64kb"
                }
            }
        });

        let config: Server = serde_json::from_value(json_config).unwrap();

        assert_eq!(config.http.max.header_size, Some(ByteSize::kb(32)));
        assert_eq!(config.http.max.headers, Some(200));
        assert_eq!(config.http.max.header_list_size, Some(ByteSize::kb(64)));
    }

    #[test]
    fn it_json_parses_mixed_http_config_correctly() {
        let json_config = json!({
            "http": {
                "header_read_timeout": "15s",
                "max": {
                    "header_size": "16kb",
                    "headers": 150
                }
            }
        });

        let config: Server = serde_json::from_value(json_config).unwrap();

        assert_eq!(config.http.header_read_timeout, Duration::from_secs(15));
        assert_eq!(config.http.max.header_size, Some(ByteSize::kb(16)));
        assert_eq!(config.http.max.headers, Some(150));
        assert_eq!(config.http.max.header_list_size, None);
    }

    #[test]
    fn test_server_http_config_with_all_header_limits() {
        let json_config = json!({
            "http": {
                "header_read_timeout": "20s",
                "max": {
                    "header_size": "32kb", 
                    "headers": 200,
                    "header_list_size": "64kb"
                }
            }
        });

        let config: Server = serde_json::from_value(json_config).unwrap();

        assert_eq!(config.http.header_read_timeout, Duration::from_secs(20));
        assert_eq!(config.http.max.header_size, Some(ByteSize::kb(32)));
        assert_eq!(config.http.max.headers, Some(200));
        assert_eq!(config.http.max.header_list_size, Some(ByteSize::kb(64)));
    }

    #[test]
    fn test_server_http_config_partial_header_config() {
        let json_config = json!({
            "http": {
                "max": {
                    "header_size": "64kb",
                    "headers": 500
                }
            }
        });

        let config: Server = serde_json::from_value(json_config).unwrap();

        // Default timeout should be preserved
        assert_eq!(config.http.header_read_timeout, Duration::from_secs(10));
        assert_eq!(config.http.max.header_size, Some(ByteSize::kb(64)));
        assert_eq!(config.http.max.headers, Some(500));
        assert_eq!(config.http.max.header_list_size, None);
    }

    #[test]
    fn test_server_http_config_large_values() {
        let json_config = json!({
            "http": {
                "max": {
                    "header_size": "1mb",
                    "headers": 1000,
                    "header_list_size": "10mb"
                }
            }
        });

        let config: Server = serde_json::from_value(json_config).unwrap();

        assert_eq!(config.http.max.header_size, Some(ByteSize::mb(1)));
        assert_eq!(config.http.max.headers, Some(1000));
        assert_eq!(config.http.max.header_list_size, Some(ByteSize::mb(10)));
    }

    #[test]
    fn test_buildstructor_with_new_http_fields() {
        let http_config = ServerHttpConfig::builder()
            .header_read_timeout(Some(Duration::from_secs(30)))
            .max(Some(ServerHttpMaxConfig::builder()
                .header_size(Some(ByteSize::kb(48)))
                .headers(Some(300))
                .header_list_size(Some(ByteSize::kb(96)))
                .build()))
            .build();

        assert_eq!(http_config.header_read_timeout, Duration::from_secs(30));
        assert_eq!(http_config.max.header_size, Some(ByteSize::kb(48)));
        assert_eq!(http_config.max.headers, Some(300));
        assert_eq!(http_config.max.header_list_size, Some(ByteSize::kb(96)));
    }

    #[test]
    fn test_deny_unknown_fields() {
        let json_config = json!({
            "http": {
                "header_read_timeout": "10s",
                "invalid_field": "should_fail"
            }
        });

        let result: Result<Server, _> = serde_json::from_value(json_config);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown field"));
    }
}
