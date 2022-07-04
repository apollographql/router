use opentelemetry::sdk::trace::Builder;
use reqwest::Url;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use tower::BoxError;
use url::ParseError;

use crate::plugins::telemetry::config::Trace;

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

pub(crate) fn parse_url_for_endpoint(mut s: String) -> Result<Url, ParseError> {
    match Url::parse(&s) {
        Ok(url) => {
            // support the case of 'collector:4317' where url parses 'collector'
            // as the scheme instead of the host
            if url.host().is_none() && (url.scheme() != "http" || url.scheme() != "https") {
                s = format!("http://{}", s);
                Url::parse(&s)
            } else {
                Ok(url)
            }
        }
        Err(err) => {
            match err {
                // support the case of '127.0.0.1:4317' where url is interpreted
                // as a relative url without a base
                ParseError::RelativeUrlWithoutBase => {
                    s = format!("http://{}", s);
                    Url::parse(&s)
                }
                _ => Err(err),
            }
        }
    }
}

pub(crate) fn deser_endpoint<'de, D>(deserializer: D) -> Result<AgentEndpoint, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    if s == "default" {
        return Ok(AgentEndpoint::Default(AgentDefault::Default));
    }
    let url = parse_url_for_endpoint(s).map_err(serde::de::Error::custom)?;
    Ok(AgentEndpoint::Url(url))
}
