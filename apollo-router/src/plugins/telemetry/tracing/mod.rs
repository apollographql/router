use crate::plugins::telemetry::config::Trace;
use opentelemetry::sdk::trace::Builder;
use tower::BoxError;

pub mod apollo;
pub mod apollo_telemetry;
pub mod datadog;
pub mod jaeger;
pub mod otlp;
pub mod zipkin;

pub trait TracingConfigurator {
    fn apply(&self, builder: Builder, trace_config: &Trace) -> Result<Builder, BoxError>;
}
