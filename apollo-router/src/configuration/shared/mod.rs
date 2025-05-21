use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::traffic_shaping::Http2Config;

#[derive(PartialEq, Debug, Clone, Default, Deserialize, JsonSchema, buildstructor::Builder)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Client {
    pub(crate) experimental_http2: Option<Http2Config>,
    pub(crate) dns_resolution_strategy: Option<DnsResolutionStrategy>,
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
