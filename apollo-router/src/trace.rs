use std::sync::Arc;

use opentelemetry::{sdk::trace::BatchSpanProcessor, trace::TracerProvider};
use std::str::FromStr;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use crate::{
    configuration::{self, Configuration, OpenTelemetry},
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

            let provider = opentelemetry::sdk::trace::TracerProvider::builder()
                .with_span_processor(batch)
                .build();

            let tracer = provider.tracer("opentelemetry-jaeger", Some(env!("CARGO_PKG_VERSION")));
            let _ = opentelemetry::global::set_tracer_provider(provider);

            let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

            opentelemetry::global::set_error_handler(handle_error)?;
            Ok(Arc::new(subscriber.with(telemetry)))
        }
        #[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
        Some(OpenTelemetry::Otlp(configuration::otlp::Otlp::Tracing(tracing))) => {
            let tracer = if let Some(tracing) = tracing.as_ref() {
                tracing.tracer()?
            } else {
                configuration::otlp::Tracing::tracer_from_env()?
            };
            let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
            opentelemetry::global::set_error_handler(handle_error)?;
            Ok(Arc::new(subscriber.with(telemetry)))
        }
        None => Ok(Arc::new(subscriber)),
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
