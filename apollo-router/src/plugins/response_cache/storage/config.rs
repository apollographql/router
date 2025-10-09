use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use crate::configuration::RedisCache;
use crate::configuration::TlsClient;
use crate::configuration::default_metrics_interval;
use crate::configuration::default_required_to_start;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
/// Redis cache configuration
pub(crate) struct Config {
    /// List of URLs to the Redis cluster
    pub(crate) urls: Vec<url::Url>,

    /// Redis username if not provided in the URLs. This field takes precedence over the username in the URL
    pub(crate) username: Option<String>,
    /// Redis password if not provided in the URLs. This field takes precedence over the password in the URL
    pub(crate) password: Option<String>,

    #[serde(
        deserialize_with = "humantime_serde::deserialize",
        default = "default_fetch_timeout"
    )]
    #[schemars(with = "Option<String>", default)]
    /// Timeout for Redis fetch commands (default: 150ms)
    pub(crate) fetch_timeout: Duration,

    #[serde(
        deserialize_with = "humantime_serde::deserialize",
        default = "default_insert_timeout"
    )]
    #[schemars(with = "Option<String>", default)]
    /// Timeout for Redis insert commands (default: 500ms)
    ///
    /// Inserts are processed asynchronously, so this will not affect response duration.
    pub(crate) insert_timeout: Duration,

    #[serde(
        deserialize_with = "humantime_serde::deserialize",
        default = "default_invalidate_timeout"
    )]
    #[schemars(with = "Option<String>", default)]
    /// Timeout for Redis invalidation commands (default: 1s)
    pub(crate) invalidate_timeout: Duration,

    #[serde(
        deserialize_with = "humantime_serde::deserialize",
        default = "default_maintenance_timeout"
    )]
    #[schemars(with = "Option<String>", default)]
    /// Timeout for Redis maintenance commands (default: 500ms)
    ///
    /// Maintenance tasks are processed asynchronously, so this will not affect response duration.
    pub(crate) maintenance_timeout: Duration,

    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "Option<String>", default)]
    /// TTL for entries
    pub(crate) ttl: Option<Duration>,

    /// namespace used to prefix Redis keys
    pub(crate) namespace: Option<String>,

    #[serde(default)]
    /// TLS client configuration
    pub(crate) tls: Option<TlsClient>,

    #[serde(default = "default_required_to_start")]
    /// Prevents the router from starting if it cannot connect to Redis
    pub(crate) required_to_start: bool,

    #[serde(default = "default_pool_size")]
    /// The size of the Redis connection pool (default: 5)
    pub(crate) pool_size: u32,

    #[serde(
        deserialize_with = "humantime_serde::deserialize",
        default = "default_metrics_interval"
    )]
    #[schemars(with = "Option<String>", default)]
    /// Interval for collecting Redis metrics (default: 1s)
    pub(crate) metrics_interval: Duration,
}

fn default_fetch_timeout() -> Duration {
    Duration::from_millis(150)
}

fn default_insert_timeout() -> Duration {
    Duration::from_millis(500)
}

fn default_invalidate_timeout() -> Duration {
    Duration::from_secs(1)
}

fn default_maintenance_timeout() -> Duration {
    Duration::from_millis(500)
}

fn default_pool_size() -> u32 {
    5
}

impl From<&Config> for RedisCache {
    fn from(value: &Config) -> Self {
        let timeout = value
            .fetch_timeout
            .max(value.insert_timeout)
            .max(value.invalidate_timeout)
            .max(value.maintenance_timeout);
        Self {
            urls: value.urls.clone(),
            username: value.username.clone(),
            password: value.password.clone(),
            timeout,
            ttl: value.ttl,
            namespace: value.namespace.clone(),
            tls: value.tls.clone(),
            required_to_start: value.required_to_start,
            reset_ttl: false,
            pool_size: value.pool_size,
            metrics_interval: value.metrics_interval,
        }
    }
}

#[cfg(all(
    test,
    any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
))]
impl Config {
    pub(crate) fn test(clustered: bool, namespace: &str) -> Self {
        let url = if clustered {
            "redis-cluster://127.0.0.1:7000"
        } else {
            "redis://127.0.0.1:6379"
        };

        serde_json_bytes::from_value(serde_json_bytes::json!({
            "urls": [url],
            "namespace": namespace,
            "pool_size": 1,
            "required_to_start": true,
            "ttl": "5m"
        }))
        .unwrap()
    }
}
