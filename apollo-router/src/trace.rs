use crate::apollo_telemetry::new_pipeline;
use std::sync::Arc;

use opentelemetry::{sdk::trace::BatchSpanProcessor, trace::TracerProvider};
use std::str::FromStr;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

#[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
use crate::configuration::otlp;
use crate::{
    configuration::{Configuration, OpenTelemetry},
    GLOBAL_ENV_FILTER,
};

pub(crate) fn try_initialize_subscriber(
    config: &Configuration,
) -> Result<Arc<dyn tracing::Subscriber + Send + Sync + 'static>, Box<dyn std::error::Error>> {
    let subscriber = tracing_subscriber::fmt::fmt()
        .with_env_filter(EnvFilter::new(
            GLOBAL_ENV_FILTER
                .get()
                .map(|x| x.as_str())
                .unwrap_or("info"),
        ))
        .finish();

    tracing::info!(
        "spaceport: {:?}, graph: {:?}",
        config.spaceport,
        config.graph
    );
    let spaceport_config = &config.spaceport;
    let graph_config = &config.graph;

    match config.opentelemetry.as_ref() {
        Some(OpenTelemetry::Jaeger(config)) => {
            let default_config = Default::default();
            let config = config.as_ref().unwrap_or(&default_config);
            let mut pipeline =
                opentelemetry_jaeger::new_pipeline().with_service_name(&config.service_name);
            if let Some(address) = config.agent_endpoint.as_ref() {
                pipeline = pipeline.with_agent_endpoint(address);
            }
            if let Some(url) = config.collector_endpoint.as_ref() {
                pipeline = pipeline.with_collector_endpoint(url.as_str());
            }
            if let Some(username) = config.username.as_ref() {
                pipeline = pipeline.with_collector_username(username);
            }
            if let Some(password) = config.password.as_ref() {
                pipeline = pipeline.with_collector_password(password);
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

            let tracer = provider.tracer("opentelemetry-jaeger", Some(env!("CARGO_PKG_VERSION")));

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
                otlp::Tracing::tracer_from_env()?
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
                tracing::info!("Adding agent telemetry");
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
                tracing::info!("Adding agent telemetry");
                Ok(Arc::new(subscriber.with(agent)))
            } else {
                Ok(Arc::new(subscriber))
            }
        }
    }
}

pub fn handle_error<T: Into<opentelemetry::global::Error>>(err: T) {
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
