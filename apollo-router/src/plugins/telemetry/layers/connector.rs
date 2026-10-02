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
use crate::plugins::telemetry::config_new::connector::events::ConnectorEvents;
use crate::plugins::telemetry::config_new::connector::instruments::ConnectorInstruments;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::instruments::StaticInstrument;
use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;
use crate::plugins::telemetry::span_factory;
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
}
