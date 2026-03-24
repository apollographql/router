use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::traffic_shaping::Http2Config;

/// Default for idle keep-alive sockets in a connection pool for HttpClientService
///
/// NOTE: the default in hyper is 90s but historically has been set much lower (5s). I couldn't
/// find a reason for such a low timeout for keep-alive sockets, so bumping it to 15s;
/// taste/adjust, but leave a comment giving justification for any new threshold
const DEFAULT_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(15);

/// HTTP client configuration
#[derive(PartialEq, Debug, Clone, Default, Deserialize, JsonSchema, buildstructor::Builder)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Client {
    /// Use HTTP/2 to communicate with the coprocessor.
    pub(crate) experimental_http2: Option<Http2Config>,
    /// Specify a DNS resolution strategy to use when resolving the coprocessor URL.
    pub(crate) dns_resolution_strategy: Option<DnsResolutionStrategy>,
    #[serde(
        deserialize_with = "humantime_serde::deserialize",
        default = "default_pool_idle_timeout"
    )]
    #[schemars(with = "String", default = "default_pool_idle_timeout")]
    /// Specify a timeout for idle sockets being kept-alive in the client's connection pool
    pub(crate) pool_idle_timeout: Option<Duration>,
}

/// Returns the hardcoded default pool idle timeout for keep-alive sockets in a client's connection
/// pool. Useful as a default for serde deserializers or other areas where this default is needed
pub(crate) fn default_pool_idle_timeout() -> Option<Duration> {
    Some(DEFAULT_POOL_IDLE_TIMEOUT)
}

#[derive(PartialEq, Default, Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DnsResolutionStrategy {
    /// Only query for `A` (IPv4) records
    Ipv4Only,
    /// Only query for `AAAA` (IPv6) records
    Ipv6Only,
    /// Query for both `A` (IPv4) and `AAAA` (IPv6) records in parallel
    Ipv4AndIpv6,
    /// Query for `AAAA` (IPv6) records first; if that fails, query for `A` (IPv4) records
    Ipv6ThenIpv4,
    #[default]
    /// Default: Query for `A` (IPv4) records first; if that fails, query for `AAAA` (IPv6) records
    Ipv4ThenIpv6,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::humantime_seconds("pool_idle_timeout: 30s", Some(Duration::from_secs(30)))]
    #[case::humantime_millis("pool_idle_timeout: 500ms", Some(Duration::from_millis(500)))]
    #[case::humantime_minutes("pool_idle_timeout: 2m", Some(Duration::from_secs(120)))]
    #[case::explicit_null("pool_idle_timeout: null", None)]
    fn test_pool_idle_timeout_deserialization(
        #[case] yaml: &str,
        #[case] expected: Option<Duration>,
    ) {
        let client: Client = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(client.pool_idle_timeout, expected);
    }

    #[test]
    fn test_pool_idle_timeout_default_when_omitted() {
        let client: Client = serde_yaml::from_str("{}").unwrap();
        assert_eq!(client.pool_idle_timeout, Some(DEFAULT_POOL_IDLE_TIMEOUT));
    }

    #[test]
    fn test_pool_idle_timeout_default_value() {
        assert_eq!(DEFAULT_POOL_IDLE_TIMEOUT, Duration::from_secs(15));
        assert_eq!(default_pool_idle_timeout(), Some(Duration::from_secs(15)));
    }

    #[test]
    fn test_client_default_has_pool_idle_timeout() {
        let client = Client::default();
        assert_eq!(client.pool_idle_timeout, None);

        let client = Client::builder().build();
        assert_eq!(client.pool_idle_timeout, None);
    }

    #[test]
    fn test_client_deny_unknown_fields() {
        let result: Result<Client, _> = serde_yaml::from_str("bogus_field: true");
        assert!(result.is_err());
    }
}
