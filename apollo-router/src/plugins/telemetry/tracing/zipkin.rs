//! Configuration for zipkin tracing.
use opentelemetry::sdk::trace::Builder;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use tower::BoxError;
use url::Url;

use super::AgentDefault;
use super::AgentEndpoint;
use crate::plugins::telemetry::config::GenericWith;
use crate::plugins::telemetry::config::Trace;
use crate::plugins::telemetry::tracing::TracingConfigurator;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[schemars(with = "String", default = "default_agent_endpoint")]
    #[serde(deserialize_with = "deser_endpoint")]
    pub endpoint: AgentEndpoint,
}

fn default_agent_endpoint() -> &'static str {
    "default"
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
        s = format!("http://{}/api/v2/spans", s);

        url = Url::parse(&s).map_err(serde::de::Error::custom)?;
    }
    Ok(AgentEndpoint::Url(url))
}

impl TracingConfigurator for Config {
    fn apply(&self, builder: Builder, trace_config: &Trace) -> Result<Builder, BoxError> {
        tracing::debug!("configuring Zipkin tracing");
        let collector_endpoint = match &self.endpoint {
            AgentEndpoint::Default(_) => None,
            AgentEndpoint::Url(url) => Some(url),
        };

        let exporter = opentelemetry_zipkin::new_pipeline()
            .with_trace_config(trace_config.into())
            .with(&trace_config.service_name, |b, n| b.with_service_name(n))
            .with(&collector_endpoint, |b, endpoint| {
                b.with_collector_endpoint(&endpoint.to_string())
            })
            .init_exporter()?;

        Ok(builder.with_batch_exporter(exporter, opentelemetry::runtime::Tokio))
    }
}
