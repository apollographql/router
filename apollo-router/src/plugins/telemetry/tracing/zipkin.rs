use crate::plugins::telemetry::config::{GenericWith, Trace};
use crate::plugins::telemetry::tracing::TracingConfigurator;
use opentelemetry::sdk::trace::Builder;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tower::BoxError;
use url::Url;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(flatten)]
    pub endpoint: Endpoint,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum Endpoint {
    Agent { endpoint: AgentEndpoint },
    Collector { endpoint: Url },
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub enum AgentEndpoint {
    Default(AgentDefault),
    Socket(SocketAddr),
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum AgentDefault {
    Default,
}

impl TracingConfigurator for Config {
    fn apply(&self, builder: Builder, trace_config: &Trace) -> Result<Builder, BoxError> {
        tracing::debug!("configuring Zipkin tracing");
        let exporter = match &self.endpoint {
            Endpoint::Agent { endpoint } => {
                let socket = match endpoint {
                    AgentEndpoint::Default(_) => None,
                    AgentEndpoint::Socket(s) => Some(s),
                };
                opentelemetry_zipkin::new_pipeline()
                    .with_trace_config(trace_config.into())
                    .with(&trace_config.service_name, |b, n| b.with_service_name(n))
                    .with(&socket, |b, s| b.with_service_address(*(*s)))
                    .init_exporter()?
            }
            Endpoint::Collector { endpoint } => opentelemetry_zipkin::new_pipeline()
                .with_trace_config(trace_config.into())
                .with(&trace_config.service_name, |b, n| b.with_service_name(n))
                .with_collector_endpoint(&endpoint.to_string())
                .init_exporter()?,
        };
        Ok(builder.with_batch_exporter(exporter, opentelemetry::runtime::Tokio))
    }
}

#[cfg(test)]
mod tests {
    use crate::plugins::telemetry::tracing::test::run_query;
    use opentelemetry::global;
    use tower::BoxError;
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Registry;

    // This test can be run manually from your IDE to help with testing otel
    // It is set to ignore by default as zipkin may not be set up
    #[ignore]
    #[tokio::test(flavor = "multi_thread")]
    async fn test_tracing() -> Result<(), BoxError> {
        tracing_subscriber::fmt().init();

        global::set_text_map_propagator(opentelemetry_zipkin::Propagator::new());
        let tracer = opentelemetry_zipkin::new_pipeline()
            .with_service_name("my_app")
            .install_batch(opentelemetry::runtime::Tokio)?;

        // Create a tracing layer with the configured tracer
        let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

        // Use the tracing subscriber `Registry`, or any other subscriber
        // that impls `LookupSpan`
        let subscriber = Registry::default().with(telemetry);

        // Trace executed code
        run_query().with_subscriber(subscriber).await;
        global::shutdown_tracer_provider();

        Ok(())
    }
}
