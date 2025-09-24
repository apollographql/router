use multimap::MultiMap;
use tower::BoxError;

use crate::Endpoint;
use crate::ListenAddr;
use crate::plugins::telemetry::apollo_exporter;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::reload::activation::Activation;
use crate::plugins::telemetry::reload::builder::Builder;

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
