use crate::plugins::telemetry::config::Trace;
use opentelemetry::sdk::trace::Builder;
use reqwest::Url;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use tower::BoxError;

pub(crate) mod apollo;
pub(crate) mod apollo_telemetry;
pub(crate) mod datadog;
pub(crate) mod jaeger;
pub(crate) mod otlp;
pub(crate) mod zipkin;

pub(crate) trait TracingConfigurator {
    fn apply(&self, builder: Builder, trace_config: &Trace) -> Result<Builder, BoxError>;
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub enum AgentEndpoint {
    Default(AgentDefault),
    Url(Url),
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum AgentDefault {
    Default,
}

pub(crate) fn deser_endpoint<'de, D>(deserializer: D) -> Result<AgentEndpoint, D::Error>
where
    D: Deserializer<'de>,
{
    let mut s = String::deserialize(deserializer)?;
    if s == "default" {
        return Ok(AgentEndpoint::Default(AgentDefault::Default));
    }
    let mut url = Url::parse(&s).map_err(serde::de::Error::custom)?;

    // support the case of 'collector:4317' where url parses 'collector'
    // as the scheme instead of the host
    if url.host().is_none() && (url.scheme() != "http" || url.scheme() != "https") {
        s = format!("http://{}", s);

        url = Url::parse(&s).map_err(serde::de::Error::custom)?;
    }
    Ok(AgentEndpoint::Url(url))
}
