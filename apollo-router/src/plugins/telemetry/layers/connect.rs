use std::collections::HashMap;
use std::sync::Arc;

use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::Context;
use crate::layers::ServiceBuilderExt;
use crate::plugins::telemetry::Telemetry;
use crate::plugins::telemetry::config;
use crate::plugins::telemetry::config_new::cache::ConnectorCacheInstruments;
use crate::plugins::telemetry::config_new::instruments::StaticInstrument;
use crate::services::connect;

/// Layer type for [Telemetry::instrument_connector_cache_layer].
#[derive(Clone)]
pub(crate) struct InstrumentConnectorCacheLayer {
    config: Arc<config::Conf>,
    static_cache_instruments: Arc<HashMap<String, StaticInstrument>>,
}

impl<S> tower::Layer<S> for InstrumentConnectorCacheLayer
where
    S: tower::Service<connect::Request, Response = connect::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = connect::BoxCloneService;

    fn layer(&self, service: S) -> Self::Service {
        let config = self.config.clone();
        let static_cache_instruments = self.static_cache_instruments.clone();
        ServiceBuilder::new()
            .map_future_with_request_data(
                move |request: &connect::Request| {
                    let connectors =
                        crate::plugins::connectors::query_plans::get_connectors(&request.context);
                    let source_name = connectors
                        .as_ref()
                        .and_then(|c| c.get(&request.service_name))
                        .map(|c| c.source_config_key())
                        .unwrap_or_default();
                    let cache_instruments = config
                        .instrumentation
                        .instruments
                        .new_connector_cache_instruments(
                            static_cache_instruments.clone(),
                            source_name,
                        );
                    (request.context.clone(), cache_instruments)
                },
                move |(context, cache_instruments): (Context, ConnectorCacheInstruments),
                      f: S::Future| async move {
                    let result = f.await;
                    if result.is_ok() {
                        cache_instruments.on_response(&context);
                    }
                    result
                },
            )
            .service(service)
            .boxed_clone()
    }
}

impl Telemetry {
    /// Returns a layer that records the response cache instruments for a connector.
    pub(crate) fn instrument_connector_cache_layer(&self) -> InstrumentConnectorCacheLayer {
        InstrumentConnectorCacheLayer {
            config: self.config.clone(),
            static_cache_instruments: self
                .builtin_instruments
                .read()
                .cache_custom_instruments
                .clone(),
        }
    }
}
