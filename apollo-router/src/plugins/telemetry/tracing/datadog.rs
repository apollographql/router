//! Configuration for datadog tracing.
use crate::plugins::telemetry::config::{GenericWith, Trace};
use crate::plugins::telemetry::tracing::TracingConfigurator;
use opentelemetry::sdk::trace::Builder;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use tower::BoxError;
use url::Url;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(deserialize_with = "deser_endpoint")]
    pub endpoint: AgentEndpoint,
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

fn deser_endpoint<'de, D>(deserializer: D) -> Result<AgentEndpoint, D::Error>
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

impl TracingConfigurator for Config {
    fn apply(&self, builder: Builder, trace_config: &Trace) -> Result<Builder, BoxError> {
        tracing::debug!("configuring Datadog tracing");
        let url = match &self.endpoint {
            AgentEndpoint::Default(_) => None,
            AgentEndpoint::Url(s) => Some(s),
        };
        let exporter = opentelemetry_datadog::new_pipeline()
            .with(&url, |b, e| {
                b.with_agent_endpoint(e.to_string().trim_end_matches('/'))
            })
            .with(&trace_config.service_name, |b, n| b.with_service_name(n))
            .with_trace_config(trace_config.into())
            .build_exporter()?;
        Ok(builder.with_batch_exporter(exporter, opentelemetry::runtime::Tokio))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_configuration() {
        let config: Config = serde_yaml::from_str("endpoint: default").unwrap();
        assert_eq!(
            AgentEndpoint::Default(AgentDefault::Default),
            config.endpoint
        );

        let config: Config = serde_yaml::from_str("endpoint: collector:1234").unwrap();
        assert_eq!(
            AgentEndpoint::Url(Url::parse("http://collector:1234").unwrap()),
            config.endpoint
        );

        let config: Config = serde_yaml::from_str("endpoint: https://collector:1234").unwrap();
        assert_eq!(
            AgentEndpoint::Url(Url::parse("https://collector:1234").unwrap()),
            config.endpoint
        );
    }
}
