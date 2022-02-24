use crate::apollo_telemetry::new_pipeline;
#[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
use crate::configuration::otlp;
#[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
use crate::configuration::otlp::Tracing;
use crate::configuration::JaegerEndpoint;
use crate::configuration::OpenTelemetry;
use crate::configuration::SpaceportConfig;
use crate::configuration::StudioGraph;
use crate::set_subscriber;
use crate::GLOBAL_ENV_FILTER;
use apollo_router_core::{register_plugin, Plugin};
use apollo_spaceport::server::ReportSpaceport;
use futures::Future;
use opentelemetry::sdk::trace::BatchSpanProcessor;
use opentelemetry::trace::TracerProvider;
use schemars::JsonSchema;
use serde::Deserialize;
use std::error::Error;
use std::fmt;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use tower::BoxError;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

#[derive(Debug)]
struct OtelError;

impl fmt::Display for OtelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OtelError")
    }
}

impl std::error::Error for OtelError {}

#[derive(Debug)]
struct Otel {
    config: Conf,
    tx: tokio::sync::mpsc::Sender<SpaceportConfig>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct Conf {
    pub spaceport: Option<SpaceportConfig>,

    pub graph: Option<StudioGraph>,

    pub opentelemetry: Option<OpenTelemetry>,
}

fn studio_graph() -> Option<StudioGraph> {
    if let Ok(apollo_key) = std::env::var("APOLLO_KEY") {
        let apollo_graph_ref = std::env::var("APOLLO_GRAPH_REF").expect(
            "cannot set up usage reporting if the APOLLO_GRAPH_REF environment variable is not set",
        );

        Some(StudioGraph {
            reference: apollo_graph_ref,
            key: apollo_key,
        })
    } else {
        None
    }
}

#[async_trait::async_trait]
impl Plugin for Otel {
    type Config = Conf;

    async fn startup(&mut self) -> Result<(), BoxError> {
        tracing::debug!("starting: {}: {}", stringify!(Otel), self.name());
        set_subscriber(self.try_initialize_subscriber()?);

        // Only check for notify if we have graph configuration
        if self.config.graph.is_some() {
            self.tx
                .send(self.config.spaceport.clone().unwrap_or_default())
                .await?;
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), BoxError> {
        tracing::debug!("shutting down: {}: {}", stringify!(Otel), self.name());
        Ok(())
    }

    fn new(mut configuration: Self::Config) -> Result<Self, BoxError> {
        tracing::info!("Otel configuration {:?}!", configuration);
        // Create graph configuration based on environment variables
        configuration.graph = studio_graph();
        tracing::info!("opentelemetry config: {:?}", configuration.opentelemetry);
        // configuration.opentelemetry = None;
        // Studio Agent Spaceport listener
        let (tx, mut rx) = tokio::sync::mpsc::channel::<SpaceportConfig>(1);

        tokio::spawn(async move {
            let mut current_listener = "".to_string();
            let mut current_operation: fn(
                msg: String,
            )
                -> Pin<Box<dyn Future<Output = bool> + Send>> = |msg| Box::pin(do_nothing(msg));

            loop {
                tokio::select! {
                    biased;
                    mopt = rx.recv() => {
                        match mopt {
                            Some(msg) => {
                                tracing::debug!(?msg);
                                // Save our target listener for later use
                                current_listener = msg.listener.clone();
                                // Configure which function to call
                                if msg.external {
                                    current_operation = |msg| Box::pin(do_nothing(msg));
                                } else {
                                    current_operation = |msg| Box::pin(do_listen(msg));
                                }
                            },
                            None => break
                        }
                    },
                    x = current_operation(current_listener.clone()) => {
                        // current_operation will only return if there is
                        // something wrong in our configuration. We don't
                        // want to terminate, so wait for a while and
                        // then try again. At some point, re-configuration
                        // will fix this.
                        tracing::debug!(%x, "current_operation");
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                };
            }
            tracing::debug!("terminating spaceport loop");
        });
        Ok(Otel {
            config: configuration,
            tx,
        })
    }
}

impl Otel {
    fn try_initialize_subscriber(
        &self,
    ) -> Result<Arc<dyn tracing::Subscriber + Send + Sync + 'static>, BoxError> {
        let subscriber = tracing_subscriber::fmt::fmt()
            .with_env_filter(EnvFilter::new(
                GLOBAL_ENV_FILTER
                    .get()
                    .map(|x| x.as_str())
                    .unwrap_or("info"),
            ))
            .finish();

        tracing::debug!(
            "spaceport: {:?}, graph: {:?}",
            self.config.spaceport,
            self.config.graph
        );
        let spaceport_config = &self.config.spaceport;
        let graph_config = &self.config.graph;

        match self.config.opentelemetry.as_ref() {
            Some(OpenTelemetry::Jaeger(config)) => {
                let default_config = Default::default();
                let config = config.as_ref().unwrap_or(&default_config);
                let mut pipeline =
                    opentelemetry_jaeger::new_pipeline().with_service_name(&config.service_name);
                match config.endpoint.as_ref() {
                    Some(JaegerEndpoint::Agent(address)) => {
                        pipeline = pipeline.with_agent_endpoint(address)
                    }
                    Some(JaegerEndpoint::Collector(url)) => {
                        pipeline = pipeline.with_collector_endpoint(url.as_str());

                        if let Some(username) = config.username.as_ref() {
                            pipeline = pipeline.with_collector_username(username);
                        }
                        if let Some(password) = config.password.as_ref() {
                            pipeline = pipeline.with_collector_password(password);
                        }
                    }
                    _ => {}
                }

                let batch_size = std::env::var("OTEL_BSP_MAX_EXPORT_BATCH_SIZE")
                    .ok()
                    .and_then(|batch_size| usize::from_str(&batch_size).ok());

                let exporter = pipeline.init_async_exporter(opentelemetry::runtime::Tokio)?;

                let batch = BatchSpanProcessor::builder(exporter, opentelemetry::runtime::Tokio)
                    .with_scheduled_delay(std::time::Duration::from_secs(1));
                let batch = if let Some(size) = batch_size {
                    batch.with_max_export_batch_size(size)
                } else {
                    batch
                }
                .build();

                let mut builder = opentelemetry::sdk::trace::TracerProvider::builder();
                if let Some(trace_config) = &config.trace_config {
                    builder = builder.with_config(trace_config.trace_config());
                }
                // If we have apollo graph configuration, then we can export statistics
                // to the apollo ingress. If we don't, we can't and so no point configuring the
                // exporter.
                if graph_config.is_some() {
                    let apollo_exporter = match new_pipeline()
                        .with_spaceport_config(spaceport_config)
                        .with_graph_config(graph_config)
                        .get_exporter()
                    {
                        Ok(x) => x,
                        Err(e) => {
                            tracing::error!("error installing spaceport telemetry: {}", e);
                            return Err(Box::new(e));
                        }
                    };
                    builder =
                        builder.with_batch_exporter(apollo_exporter, opentelemetry::runtime::Tokio)
                }

                let provider = builder.with_span_processor(batch).build();

                let tracer = provider.versioned_tracer(
                    "opentelemetry-jaeger",
                    Some(env!("CARGO_PKG_VERSION")),
                    None,
                );

                // This code will hang unless we execute from a separate
                // thread.  See:
                // https://github.com/apollographql/router/issues/331
                // https://github.com/open-telemetry/opentelemetry-rust/issues/536
                // for more details and description.
                let jh = tokio::task::spawn_blocking(|| {
                    opentelemetry::global::force_flush_tracer_provider();
                    opentelemetry::global::set_tracer_provider(provider);
                });
                futures::executor::block_on(jh)?;

                let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

                opentelemetry::global::set_error_handler(handle_error)?;

                Ok(Arc::new(subscriber.with(telemetry)))
            }
            #[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
            Some(OpenTelemetry::Otlp(otlp::Otlp::Tracing(tracing))) => {
                let tracer = if let Some(tracing) = tracing.as_ref() {
                    tracing.tracer()?
                } else {
                    Tracing::tracer_from_env()?
                };
                let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
                opentelemetry::global::set_error_handler(handle_error)?;
                // It's difficult to extend the OTLP model with an additional exporter
                // as we do when Jaeger is being used. In this case we simply add the
                // agent as a new layer and proceed from there.
                let subscriber = subscriber.with(telemetry);
                if graph_config.is_some() {
                    // Add spaceport agent as an OT pipeline
                    let tracer = match new_pipeline()
                        .with_spaceport_config(spaceport_config)
                        .with_graph_config(graph_config)
                        .install_batch()
                    {
                        Ok(t) => t,
                        Err(e) => {
                            tracing::error!("error installing spaceport telemetry: {}", e);
                            return Err(Box::new(e));
                        }
                    };
                    let agent = tracing_opentelemetry::layer().with_tracer(tracer);
                    tracing::debug!("Adding agent telemetry");
                    Ok(Arc::new(subscriber.with(agent)))
                } else {
                    Ok(Arc::new(subscriber))
                }
            }
            None => {
                if graph_config.is_some() {
                    // Add spaceport agent as an OT pipeline
                    let tracer = match new_pipeline()
                        .with_spaceport_config(spaceport_config)
                        .with_graph_config(graph_config)
                        .install_batch()
                    {
                        Ok(t) => t,
                        Err(e) => {
                            tracing::error!("error installing spaceport telemetry: {}", e);
                            return Err(Box::new(e));
                        }
                    };
                    let agent = tracing_opentelemetry::layer().with_tracer(tracer);
                    tracing::debug!("Adding agent telemetry");
                    Ok(Arc::new(subscriber.with(agent)))
                } else {
                    Ok(Arc::new(subscriber))
                }
            }
        }
    }
}

fn handle_error<T: Into<opentelemetry::global::Error>>(err: T) {
    match err.into() {
        opentelemetry::global::Error::Trace(err) => {
            tracing::error!("OpenTelemetry trace error occurred: {}", err)
        }
        opentelemetry::global::Error::Other(err_msg) => {
            tracing::error!("OpenTelemetry error occurred: {}", err_msg)
        }
        other => {
            tracing::error!("OpenTelemetry error occurred: {:?}", other)
        }
    }
}

// For use when we have an external collector. Makes selecting over
// events simpler
async fn do_nothing(_addr_str: String) -> bool {
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
    }
    #[allow(unreachable_code)]
    false
}

// For use when we have an internal collector.
async fn do_listen(addr_str: String) -> bool {
    tracing::debug!("spawning an internal spaceport");
    // Spawn a spaceport server to handle statistics
    let addr = match addr_str.parse() {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("could not parse spaceport address: {}", e);
            return false;
        }
    };

    let spaceport = ReportSpaceport::new(addr);

    if let Err(e) = spaceport.serve().await {
        match e.source() {
            Some(source) => {
                tracing::warn!("spaceport did not terminate normally: {}", source);
            }
            None => {
                tracing::warn!("spaceport did not terminate normally: {}", e);
            }
        }
        return false;
    }
    true
}

register_plugin!("com.apollo", "otel", Otel);

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use std::str::FromStr;

    #[tokio::test]
    async fn plugin_registered() {
        apollo_router_core::plugins()
            .get("com.apollo.otel")
            .expect("Plugin not found")
            .create_instance(&Value::from_str("{\"name\":\"Bob\"}").unwrap())
            .unwrap();
    }
}
