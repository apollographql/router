use std::collections::HashMap;
use std::sync::Arc;

use ::tracing::Span;
use opentelemetry::KeyValue;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::Context;
use crate::layers::ServiceBuilderExt;
use crate::plugins::telemetry::Telemetry;
use crate::plugins::telemetry::config;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::apollo::instruments::ApolloConnectorInstruments;
use crate::plugins::telemetry::config_new::cache::ConnectorCacheInstruments;
use crate::plugins::telemetry::config_new::connector::events::ConnectorEvents;
use crate::plugins::telemetry::config_new::connector::instruments::ConnectorInstruments;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::instruments::StaticInstrument;
use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;
use crate::plugins::telemetry::span_factory;
use crate::services::connect;
use crate::services::connector;

/// Layer type for [Telemetry::instrument_connector_layer].
#[derive(Clone)]
pub(crate) struct InstrumentConnectorLayer {
    config: Arc<config::Conf>,
    static_connector_instruments: Arc<HashMap<String, StaticInstrument>>,
    static_apollo_connector_instruments: Arc<HashMap<String, StaticInstrument>>,
}

impl InstrumentConnectorLayer {
    fn new(
        config: Arc<config::Conf>,
        static_connector_instruments: Arc<HashMap<String, StaticInstrument>>,
        static_apollo_connector_instruments: Arc<HashMap<String, StaticInstrument>>,
    ) -> Self {
        Self {
            config,
            static_connector_instruments,
            static_apollo_connector_instruments,
        }
    }
}

impl<S> tower::Layer<S> for InstrumentConnectorLayer
where
    S: tower::Service<
            connector::request_service::Request,
            Response = connector::request_service::Response,
            Error = BoxError,
        > + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = connector::request_service::BoxCloneService;

    fn layer(&self, service: S) -> Self::Service {
        let req_fn_config = self.config.clone();
        let res_fn_config = self.config.clone();
        let static_connector_instruments = self.static_connector_instruments.clone();
        let static_apollo_connector_instruments = self.static_apollo_connector_instruments.clone();
        ServiceBuilder::new()
            .instrument(move |req: &connector::request_service::Request| {
                span_factory::create_connector(req.connector.source_config_key().as_str())
            })
            .map_future_with_request_data(
                move |request: &connector::request_service::Request| {
                    let custom_instruments = req_fn_config
                        .instrumentation
                        .instruments
                        .new_connector_instruments(static_connector_instruments.clone());
                    custom_instruments.on_request(request);
                    let apollo_instruments = req_fn_config
                        .instrumentation
                        .instruments
                        .new_apollo_connector_instruments(
                            static_apollo_connector_instruments.clone(),
                            req_fn_config.apollo.clone(),
                        );
                    apollo_instruments.on_request(request);
                    let mut custom_events =
                        req_fn_config.instrumentation.events.new_connector_events();
                    custom_events.on_request(request);

                    let custom_span_attributes = req_fn_config
                        .instrumentation
                        .spans
                        .connector
                        .attributes
                        .on_request(request);

                    (
                        request.context.clone(),
                        custom_instruments,
                        apollo_instruments,
                        custom_events,
                        custom_span_attributes,
                    )
                },
                move |(
                    context,
                    custom_instruments,
                    apollo_connector_instruments,
                    mut custom_events,
                    custom_span_attributes,
                ): (
                    Context,
                    ConnectorInstruments,
                    ApolloConnectorInstruments,
                    ConnectorEvents,
                    Vec<KeyValue>,
                ),
                      f| {
                    let conf = res_fn_config.clone();
                    async move {
                        let span = Span::current();
                        span.set_span_dyn_attributes(custom_span_attributes);

                        let result = f.await;
                        match &result {
                            Ok(response) => {
                                span.set_span_dyn_attributes(
                                    conf.instrumentation
                                        .spans
                                        .connector
                                        .attributes
                                        .on_response(response),
                                );
                                custom_instruments.on_response(response);
                                apollo_connector_instruments.on_response(response);
                                custom_events.on_response(response);
                            }
                            Err(err) => {
                                span.set_span_dyn_attributes(
                                    conf.instrumentation
                                        .spans
                                        .connector
                                        .attributes
                                        .on_error(err, &context),
                                );
                                custom_instruments.on_error(err, &context);
                                apollo_connector_instruments.on_error(err, &context);
                                custom_events.on_error(err, &context);
                            }
                        }
                        result
                    }
                },
            )
            .service(service)
            .boxed_clone()
    }
}

/// Layer type for [Telemetry::connector_cache_layer].
#[derive(Clone)]
pub(crate) struct ConnectorCacheLayer {
    config: Arc<config::Conf>,
    static_cache_instruments: Arc<HashMap<String, StaticInstrument>>,
}

impl ConnectorCacheLayer {
    fn new(
        config: Arc<config::Conf>,
        static_cache_instruments: Arc<HashMap<String, StaticInstrument>>,
    ) -> Self {
        Self {
            config,
            static_cache_instruments,
        }
    }
}

impl<S> tower::Layer<S> for ConnectorCacheLayer
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
                    // Cache instruments are per connector source, and the source is only
                    // reachable through the query plan's connector map at this layer.
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
                      f| async move {
                    let result: Result<connect::Response, BoxError> = f.await;
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
    /// Returns a layer that instruments a connector request service with both Apollo and custom
    /// instrumentation.
    pub(crate) fn instrument_connector_layer(&self) -> InstrumentConnectorLayer {
        let static_connector_instruments = self
            .builtin_instruments
            .read()
            .connector_custom_instruments
            .clone();
        let static_apollo_connector_instruments = self
            .builtin_instruments
            .read()
            .apollo_connector_instruments
            .clone();
        InstrumentConnectorLayer::new(
            self.config.clone(),
            static_connector_instruments,
            static_apollo_connector_instruments,
        )
    }

    /// Returns a layer that records connector response-cache instruments. This sits at the
    /// `connect::` level rather than the request-service level, because a connector cache hit
    /// is resolved above the fan-out into per-source HTTP requests.
    pub(crate) fn connector_cache_layer(&self) -> ConnectorCacheLayer {
        let static_cache_instruments = self
            .builtin_instruments
            .read()
            .cache_custom_instruments
            .clone();
        ConnectorCacheLayer::new(self.config.clone(), static_cache_instruments)
    }
}
