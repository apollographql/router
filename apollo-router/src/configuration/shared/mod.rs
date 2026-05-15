use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::traffic_shaping::Http2Config;

/// Default timeout for HTTP/2 keep-alive pings in HttpClientService
///
/// NOTE: hyper_util's default keep-alive timeout is 20s, so we use the same value here
pub(crate) const DEFAULT_HTTP2_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(20);

/// HTTP client configuration
#[derive(PartialEq, Debug, Clone, Default, Deserialize, JsonSchema, buildstructor::Builder)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Client {
    /// Use HTTP/2 to communicate with the coprocessor.
    pub(crate) experimental_http2: Option<Http2Config>,

    /// Specify a DNS resolution strategy to use when resolving the coprocessor URL.
    pub(crate) dns_resolution_strategy: Option<DnsResolutionStrategy>,

    /// Configure the interval for HTTP/2 keep-alive pings. Requires HTTP/2 to be enabled. If
    /// unset (the default), keep-alive pings are disabled.
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "Option<String>", default)]
    pub(crate) experimental_http2_keep_alive_interval: Option<Duration>,

    /// Configure the timeout for HTTP/2 keep-alive pings. Requires HTTP/2 to be enabled and
    /// `experimental_http2_keep_alive_interval` to be set. Defaults to 20 seconds.
    // NB: can't make this non-optional due to the builder, but this gets
    // `unwrap_or(DEFAULT_HTTP2_KEEP_ALIVE_TIMEOUT)`'ed at the callsite.
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "Option<String>", default)]
    pub(crate) experimental_http2_keep_alive_timeout: Option<Duration>,
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

    #[test]
    fn test_client_deny_unknown_fields() {
        let result: Result<Client, _> = serde_yaml::from_str("bogus_field: true");
        assert!(result.is_err());
    }

    #[rstest]
    #[case::humantime_seconds(
        "experimental_http2_keep_alive_interval: 30s",
        Some(Duration::from_secs(30))
    )]
    #[case::humantime_millis(
        "experimental_http2_keep_alive_interval: 500ms",
        Some(Duration::from_millis(500))
    )]
    #[case::explicit_null("experimental_http2_keep_alive_interval: null", None)]
    #[case::omitted("{}", None)]
    fn test_keep_alive_interval_deserialization(
        #[case] yaml: &str,
        #[case] expected: Option<Duration>,
    ) {
        let client: Client = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(client.experimental_http2_keep_alive_interval, expected);
    }

    #[rstest]
    #[case::humantime_seconds(
        "experimental_http2_keep_alive_timeout: 10s",
        Some(Duration::from_secs(10))
    )]
    #[case::humantime_millis(
        "experimental_http2_keep_alive_timeout: 500ms",
        Some(Duration::from_millis(500))
    )]
    #[case::explicit_null("experimental_http2_keep_alive_timeout: null", None)]
    #[case::omitted("{}", None)]
    fn test_keep_alive_timeout_deserialization(
        #[case] yaml: &str,
        #[case] expected: Option<Duration>,
    ) {
        let client: Client = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(client.experimental_http2_keep_alive_timeout, expected);
    }
}
