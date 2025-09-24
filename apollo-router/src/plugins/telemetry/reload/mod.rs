use std::io::IsTerminal;

use crate::plugins::telemetry::apollo_exporter;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::otel::PreSampledTracer;
use crate::plugins::telemetry::reload::activation::Activation;
use crate::plugins::telemetry::reload::builder::Builder;
use crate::{Endpoint, ListenAddr};
use multimap::MultiMap;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TracerProvider;
use tower::BoxError;
use tracing_subscriber::layer::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub(crate) mod activation;
pub(crate) mod builder;
pub(crate) mod otel;

pub(crate) fn prepare(
    previous_config: &Option<Conf>,
    config: &Conf,
) -> Result<
    (
        Activation,
        MultiMap<ListenAddr, Endpoint>,
        apollo_exporter::Sender,
    ),
    BoxError,
> {
    Builder::new(previous_config, config).build()
}
