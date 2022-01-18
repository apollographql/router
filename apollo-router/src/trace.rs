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
    // XXX Seems bogus that we have set a subscriber in src/main.rs and yet
    // create another one here that may/will have a different configuration.
    // We should check if there is one and if not, make this the default...
    let subscriber = tracing_subscriber::fmt::fmt()
        .with_env_filter(EnvFilter::new(
            GLOBAL_ENV_FILTER
                .get()
                .map(|x| x.as_str())
                .unwrap_or("info"),
        ))
        .json()
        .finish();

    tracing::info!("config: {:?}", config.studio);
    let studio_config = &config.studio;

    match config.opentelemetry.as_ref() {
        Some(OpenTelemetry::Jaeger(config)) => {
            let default_config = Default::default();
            let config = config.as_ref().unwrap_or(&default_config);
            let mut pipeline =
                opentelemetry_jaeger::new_pipeline().with_service_name(&config.service_name);
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
            // Add an apollo exporter into the mix
            let apollo_exporter = match new_pipeline()
                .with_studio_config(studio_config)
                .get_exporter()
            {
                Ok(x) => x,
                Err(e) => {
                    tracing::error!("error installing studio telemetry: {}", e);
                    return Err(Box::new(e));
                }
            };
            let provider = builder
                .with_span_processor(batch)
                .with_batch_exporter(apollo_exporter, opentelemetry::runtime::Tokio)
                .build();

            let tracer = provider.tracer("opentelemetry-jaeger", Some(env!("CARGO_PKG_VERSION")));
            // The call to set_tracer_provider() manipulate a sync RwLock.
            // Even though this code is sync, it is called from within an
            // async context. If we don't do this in a separate thread,
            // it will cause issues with the async runtime that prevents
            // the router from working correctly.
            let _ = std::thread::spawn(|| {
                opentelemetry::global::set_tracer_provider(provider);
            })
            .join();

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
            Ok(Arc::new(subscriber.with(telemetry)))
        }
        None => {
            // Add studio agent as an OT pipeline
            let tracer = match new_pipeline()
                .with_studio_config(studio_config)
                .install_batch()
            {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("error installing studio telemetry: {}", e);
                    return Err(Box::new(e));
                }
            };
            let agent = tracing_opentelemetry::layer().with_tracer(tracer);
            tracing::info!("Adding agent telemetry");
            Ok(Arc::new(subscriber.with(agent)))
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
