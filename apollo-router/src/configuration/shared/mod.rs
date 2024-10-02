use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::traffic_shaping::Http2Config;

#[derive(PartialEq, Default, Debug, Clone, Deserialize, JsonSchema, buildstructor::Builder)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Client {
    pub(crate) experimental_http2: Option<Http2Config>,
    pub(crate) dns_resolution_strategy: Option<DnsResolutionStrategy>,
}

#[derive(PartialEq, Default, Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DnsResolutionStrategy {
    /// Only query for A (Ipv4) records
    Ipv4Only,
    /// Only query for AAAA (Ipv6) records
    Ipv6Only,
    /// Query for A and AAAA in parallel
    Ipv4AndIpv6,
    /// Query for Ipv6 if that fails, query for Ipv4
    Ipv6ThenIpv4,
    #[default]
    /// Query for Ipv4 if that fails, query for Ipv6 (default)
    Ipv4ThenIpv6,
}
