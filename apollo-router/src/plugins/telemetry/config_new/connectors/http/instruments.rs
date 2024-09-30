//! Instruments related to Connectors.

use std::collections::HashMap;
use std::sync::Arc;

use opentelemetry::metrics::MeterProvider;
use opentelemetry_api::metrics::Unit;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::time::Instant;
use tower::BoxError;

use crate::metrics;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::connectors::http::attributes::ConnectorHttpAttributes;
use crate::plugins::telemetry::config_new::connectors::http::selectors::ConnectorHttpSelector;
use crate::plugins::telemetry::config_new::connectors::http::selectors::ConnectorHttpValue;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::CustomHistogram;
use crate::plugins::telemetry::config_new::instruments::CustomHistogramInner;
use crate::plugins::telemetry::config_new::instruments::CustomInstruments;
use crate::plugins::telemetry::config_new::instruments::DefaultedStandardInstrument;
use crate::plugins::telemetry::config_new::instruments::Increment;
use crate::plugins::telemetry::config_new::instruments::Instrument;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::instruments::StaticInstrument;
use crate::plugins::telemetry::config_new::instruments::HTTP_CLIENT_REQUEST_BODY_SIZE_METRIC;
use crate::plugins::telemetry::config_new::instruments::HTTP_CLIENT_REQUEST_DURATION_METRIC;
use crate::plugins::telemetry::config_new::instruments::HTTP_CLIENT_RESPONSE_BODY_SIZE_METRIC;
use crate::plugins::telemetry::config_new::instruments::METER_NAME;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::Context;

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ConnectorInstrumentsConfig {
    /// Histogram of client request duration
    #[serde(rename = "http.client.request.duration")]
    http_client_request_duration:
        DefaultedStandardInstrument<Extendable<ConnectorHttpAttributes, ConnectorHttpSelector>>,

    /// Histogram of client request body size
    #[serde(rename = "http.client.request.body.size")]
    http_client_request_body_size:
        DefaultedStandardInstrument<Extendable<ConnectorHttpAttributes, ConnectorHttpSelector>>,

    /// Histogram of client response body size
    #[serde(rename = "http.client.response.body.size")]
    http_client_response_body_size:
        DefaultedStandardInstrument<Extendable<ConnectorHttpAttributes, ConnectorHttpSelector>>,
}

impl DefaultForLevel for ConnectorInstrumentsConfig {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        self.http_client_request_duration
            .defaults_for_level(requirement_level, kind);
        self.http_client_request_body_size
            .defaults_for_level(requirement_level, kind);
        self.http_client_response_body_size
            .defaults_for_level(requirement_level, kind);
    }
}

pub(crate) struct ConnectorHttpInstruments {
    http_client_request_duration: Option<
        CustomHistogram<HttpRequest, HttpResponse, ConnectorHttpAttributes, ConnectorHttpSelector>,
    >,
    http_client_request_body_size: Option<
        CustomHistogram<HttpRequest, HttpResponse, ConnectorHttpAttributes, ConnectorHttpSelector>,
    >,
    http_client_response_body_size: Option<
        CustomHistogram<HttpRequest, HttpResponse, ConnectorHttpAttributes, ConnectorHttpSelector>,
    >,
    custom: ConnectorHttpCustomInstruments,
}

impl ConnectorHttpInstruments {
    pub(crate) fn new(
        config: &Extendable<
            ConnectorInstrumentsConfig,
            Instrument<ConnectorHttpAttributes, ConnectorHttpSelector, ConnectorHttpValue>,
        >,
        static_instruments: Arc<HashMap<String, StaticInstrument>>,
    ) -> Self {
        let http_client_request_duration =
            config
                .attributes
                .http_client_request_duration
                .is_enabled()
                .then(|| {
                    let mut nb_attributes = 0;
                    let selectors = match &config.attributes.http_client_request_duration {
                        DefaultedStandardInstrument::Bool(_)
                        | DefaultedStandardInstrument::Unset => None,
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            nb_attributes = attributes.custom.len();
                            Some(attributes.clone())
                        }
                    };
                    CustomHistogram {
                        inner: Mutex::new(CustomHistogramInner {
                            increment: Increment::Duration(Instant::now()),
                            condition: Condition::True,
                            histogram: Some(static_instruments
                                .get(HTTP_CLIENT_REQUEST_DURATION_METRIC)
                                .expect(
                                    "cannot get static instrument for connector; this should not happen",
                                )
                                .as_histogram()
                                .cloned()
                                .expect(
                                    "cannot convert instrument to histogram for connector; this should not happen",
                                )
                            ),
                            attributes: Vec::with_capacity(nb_attributes),
                            selector: None,
                            selectors,
                            updated: false,
                        }),
                    }
                });
        let http_client_request_body_size =
            config
                .attributes
                .http_client_request_body_size
                .is_enabled()
                .then(|| {
                    let mut nb_attributes = 0;
                    let selectors = match &config.attributes.http_client_request_body_size {
                        DefaultedStandardInstrument::Bool(_)
                        | DefaultedStandardInstrument::Unset => None,
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            nb_attributes = attributes.custom.len();
                            Some(attributes.clone())
                        }
                    };
                    CustomHistogram {
                        inner: Mutex::new(CustomHistogramInner {
                            increment: Increment::Custom(None),
                            condition: Condition::True,
                            histogram: Some(static_instruments
                                .get(HTTP_CLIENT_REQUEST_BODY_SIZE_METRIC)
                                .expect(
                                    "cannot get static instrument for connector; this should not happen",
                                )
                                .as_histogram()
                                .cloned()
                                .expect(
                                    "cannot convert instrument to histogram for connector; this should not happen",
                                )
                            ),
                            attributes: Vec::with_capacity(nb_attributes),
                            selector: Some(Arc::new(ConnectorHttpSelector::ConnectorRequestHeader {
                                connector_http_request_header: "content-length".to_string(),
                                redact: None,
                                default: None,
                            })),
                            selectors,
                            updated: false,
                        }),
                    }
                });
        let http_client_response_body_size =
            config
                .attributes
                .http_client_response_body_size
                .is_enabled()
                .then(|| {
                    let mut nb_attributes = 0;
                    let selectors = match &config.attributes.http_client_response_body_size {
                        DefaultedStandardInstrument::Bool(_)
                        | DefaultedStandardInstrument::Unset => None,
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            nb_attributes = attributes.custom.len();
                            Some(attributes.clone())
                        }
                    };
                    CustomHistogram {
                        inner: Mutex::new(CustomHistogramInner {
                            increment: Increment::Custom(None),
                            condition: Condition::True,
                            histogram: Some(static_instruments
                                .get(HTTP_CLIENT_RESPONSE_BODY_SIZE_METRIC)
                                .expect(
                                    "cannot get static instrument for connector; this should not happen",
                                )
                                .as_histogram()
                                .cloned()
                                .expect(
                                    "cannot convert instrument to histogram for connector; this should not happen",
                                )
                            ),
                            attributes: Vec::with_capacity(nb_attributes),
                            selector: Some(Arc::new(ConnectorHttpSelector::ConnectorResponseHeader {
                                connector_http_response_header: "content-length".to_string(),
                                redact: None,
                                default: None,
                            })),
                            selectors,
                            updated: false,
                        }),
                    }
                });
        ConnectorHttpInstruments {
            http_client_request_duration,
            http_client_request_body_size,
            http_client_response_body_size,
            custom: CustomInstruments::new(&config.custom, static_instruments),
        }
    }

    pub(crate) fn new_builtin(
        config: &Extendable<
            ConnectorInstrumentsConfig,
            Instrument<ConnectorHttpAttributes, ConnectorHttpSelector, ConnectorHttpValue>,
        >,
    ) -> HashMap<String, StaticInstrument> {
        let meter = metrics::meter_provider().meter(METER_NAME);
        let mut static_instruments = HashMap::with_capacity(3);

        if config.attributes.http_client_request_duration.is_enabled() {
            static_instruments.insert(
                HTTP_CLIENT_REQUEST_DURATION_METRIC.to_string(),
                StaticInstrument::Histogram(
                    meter
                        .f64_histogram(HTTP_CLIENT_REQUEST_DURATION_METRIC)
                        .with_unit(Unit::new("s"))
                        .with_description("Duration of HTTP client requests.")
                        .init(),
                ),
            );
        }

        if config.attributes.http_client_request_body_size.is_enabled() {
            static_instruments.insert(
                HTTP_CLIENT_REQUEST_BODY_SIZE_METRIC.to_string(),
                StaticInstrument::Histogram(
                    meter
                        .f64_histogram(HTTP_CLIENT_REQUEST_BODY_SIZE_METRIC)
                        .with_unit(Unit::new("By"))
                        .with_description("Size of HTTP client request bodies.")
                        .init(),
                ),
            );
        }

        if config
            .attributes
            .http_client_response_body_size
            .is_enabled()
        {
            static_instruments.insert(
                HTTP_CLIENT_RESPONSE_BODY_SIZE_METRIC.to_string(),
                StaticInstrument::Histogram(
                    meter
                        .f64_histogram(HTTP_CLIENT_RESPONSE_BODY_SIZE_METRIC)
                        .with_unit(Unit::new("By"))
                        .with_description("Size of HTTP client response bodies.")
                        .init(),
                ),
            );
        }

        static_instruments
    }
}

impl Instrumented for ConnectorHttpInstruments {
    type Request = HttpRequest;
    type Response = HttpResponse;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        if let Some(http_client_request_duration) = &self.http_client_request_duration {
            http_client_request_duration.on_request(request);
        }
        if let Some(http_client_request_body_size) = &self.http_client_request_body_size {
            http_client_request_body_size.on_request(request);
        }
        if let Some(http_client_response_body_size) = &self.http_client_response_body_size {
            http_client_response_body_size.on_request(request);
        }
        self.custom.on_request(request);
    }

    fn on_response(&self, response: &Self::Response) {
        if let Some(http_client_request_duration) = &self.http_client_request_duration {
            http_client_request_duration.on_response(response);
        }
        if let Some(http_client_request_body_size) = &self.http_client_request_body_size {
            http_client_request_body_size.on_response(response);
        }
        if let Some(http_client_response_body_size) = &self.http_client_response_body_size {
            http_client_response_body_size.on_response(response);
        }
        self.custom.on_response(response);
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        if let Some(http_client_request_duration) = &self.http_client_request_duration {
            http_client_request_duration.on_error(error, ctx);
        }
        if let Some(http_client_request_body_size) = &self.http_client_request_body_size {
            http_client_request_body_size.on_error(error, ctx);
        }
        if let Some(http_client_response_body_size) = &self.http_client_response_body_size {
            http_client_response_body_size.on_error(error, ctx);
        }
        self.custom.on_error(error, ctx);
    }
}

pub(crate) type ConnectorHttpCustomInstruments = CustomInstruments<
    HttpRequest,
    HttpResponse,
    ConnectorHttpAttributes,
    ConnectorHttpSelector,
    ConnectorHttpValue,
>;
