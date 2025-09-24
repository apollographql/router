//! Reload support for telemetry
//!
//! Telemetry reloading is difficult because it modifies global state. Plugins may error as they are
//! initialized so it is not possible to modify the global state there without risking that a
//! later plugin may cause the new pipeline to fail.
//!
//! Instead, once all plugins are intialised activate on `PluginPrivate` is called which commits the
//! telemtry changes. `activate ` is not failable, so we are good to commit to global state.
//!
//! This module is divided into submodules:
//! * otel - deals with global state + legacy metrics layer (to be removed in 3.0)
//! * activation - state to be applied when activate is called. Will set meter and tracing providers.
//! * builder - from config determines what has changed and pulls together information needed to serve
//!   telemetry if activate is reached.
//! * metrics - support for building meter providers from config
//! * tracing - support for building trace providers from config
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
pub(crate) mod metrics;
pub(crate) mod otel;
pub(crate) mod tracing;

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
