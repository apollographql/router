//! Configuration for datadog tracing.
use crate::plugins::telemetry::config::{GenericWith, Trace};
use crate::plugins::telemetry::tracing::TracingConfigurator;
use opentelemetry::sdk::trace::Builder;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tower::BoxError;

use super::{deser_endpoint, AgentEndpoint};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(deserialize_with = "deser_endpoint")]
    #[schemars(with = "String", default = "default_agent_endpoint")]
    pub endpoint: AgentEndpoint,
}
const fn default_agent_endpoint() -> &'static str {
    "default"
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
    use reqwest::Url;

    use crate::plugins::telemetry::tracing::AgentDefault;

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
