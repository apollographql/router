use std::collections::HashMap;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::sync::Arc;

use opentelemetry::metrics::Unit;
use opentelemetry_api::metrics::Counter;
use opentelemetry_api::metrics::Histogram;
use opentelemetry_api::metrics::MeterProvider;
use opentelemetry_api::metrics::UpDownCounter;
use opentelemetry_api::KeyValue;
use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
use opentelemetry_semantic_conventions::trace::SERVER_ADDRESS;
use opentelemetry_semantic_conventions::trace::SERVER_PORT;
use opentelemetry_semantic_conventions::trace::URL_SCHEME;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::Value;
use tokio::time::Instant;
use tower::BoxError;

use super::attributes::HttpServerAttributes;
use super::cache::attributes::CacheAttributes;
use super::cache::CacheInstruments;
use super::cache::CacheInstrumentsConfig;
use super::cache::CACHE_METRIC;
use super::graphql::selectors::ListLength;
use super::graphql::GraphQLInstruments;
use super::graphql::FIELD_EXECUTION;
use super::graphql::FIELD_LENGTH;
use super::selectors::CacheKind;
use super::DefaultForLevel;
use super::Selector;
use crate::metrics;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::cost::CostInstruments;
use crate::plugins::telemetry::config_new::cost::CostInstrumentsConfig;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::graphql::attributes::GraphQLAttributes;
use crate::plugins::telemetry::config_new::graphql::selectors::GraphQLSelector;
use crate::plugins::telemetry::config_new::graphql::selectors::GraphQLValue;
use crate::plugins::telemetry::config_new::graphql::GraphQLInstrumentsConfig;
use crate::plugins::telemetry::config_new::selectors::RouterSelector;
use crate::plugins::telemetry::config_new::selectors::RouterValue;
use crate::plugins::telemetry::config_new::selectors::SubgraphSelector;
use crate::plugins::telemetry::config_new::selectors::SubgraphValue;
use crate::plugins::telemetry::config_new::selectors::SupergraphSelector;
use crate::plugins::telemetry::config_new::selectors::SupergraphValue;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::Context;

pub(crate) const METER_NAME: &str = "apollo/router";

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct InstrumentsConfig {
    /// The attributes and instruments to include by default in instruments based on their level as specified in the otel semantic conventions and Apollo documentation.
    pub(crate) default_requirement_level: DefaultAttributeRequirementLevel,

    /// Router service instruments. For more information see documentation on Router lifecycle.
    pub(crate) router: Extendable<
        RouterInstrumentsConfig,
        Instrument<RouterAttributes, RouterSelector, RouterValue>,
    >,
    /// Supergraph service instruments. For more information see documentation on Router lifecycle.
    pub(crate) supergraph: Extendable<
        SupergraphInstrumentsConfig,
        Instrument<SupergraphAttributes, SupergraphSelector, SupergraphValue>,
    >,
    /// Subgraph service instruments. For more information see documentation on Router lifecycle.
    pub(crate) subgraph: Extendable<
        SubgraphInstrumentsConfig,
        Instrument<SubgraphAttributes, SubgraphSelector, SubgraphValue>,
    >,
    /// GraphQL response field instruments.
    pub(crate) graphql: Extendable<
        GraphQLInstrumentsConfig,
        Instrument<GraphQLAttributes, GraphQLSelector, GraphQLValue>,
    >,
    /// Cache instruments
    pub(crate) cache: Extendable<
        CacheInstrumentsConfig,
        Instrument<CacheAttributes, SubgraphSelector, SubgraphValue>,
    >,
}

const HTTP_SERVER_REQUEST_DURATION_METRIC: &str = "http.server.request.duration";
const HTTP_SERVER_REQUEST_BODY_SIZE_METRIC: &str = "http.server.request.body.size";
const HTTP_SERVER_RESPONSE_BODY_SIZE_METRIC: &str = "http.server.response.body.size";
const HTTP_SERVER_ACTIVE_REQUESTS: &str = "http.server.active_requests";

const HTTP_CLIENT_REQUEST_DURATION_METRIC: &str = "http.client.request.duration";
const HTTP_CLIENT_REQUEST_BODY_SIZE_METRIC: &str = "http.client.request.body.size";
const HTTP_CLIENT_RESPONSE_BODY_SIZE_METRIC: &str = "http.client.response.body.size";

impl InstrumentsConfig {
    /// Update the defaults for spans configuration regarding the `default_attribute_requirement_level`
    pub(crate) fn update_defaults(&mut self) {
        self.router
            .attributes
            .defaults_for_levels(self.default_requirement_level, TelemetryDataKind::Metrics);
        self.supergraph
            .defaults_for_levels(self.default_requirement_level, TelemetryDataKind::Metrics);
        self.subgraph
            .defaults_for_levels(self.default_requirement_level, TelemetryDataKind::Metrics);
        self.graphql
            .defaults_for_levels(self.default_requirement_level, TelemetryDataKind::Metrics);
    }

    pub(crate) fn new_builtin_router_instruments(&self) -> HashMap<String, StaticInstrument> {
        let meter = metrics::meter_provider().meter(METER_NAME);
        let mut static_instruments = HashMap::with_capacity(self.router.custom.len());

        if self
            .router
            .attributes
            .http_server_request_duration
            .is_enabled()
        {
            static_instruments.insert(
                HTTP_SERVER_REQUEST_DURATION_METRIC.to_string(),
                StaticInstrument::Histogram(
                    meter
                        .f64_histogram(HTTP_SERVER_REQUEST_DURATION_METRIC)
                        .with_unit(Unit::new("s"))
                        .with_description("Duration of HTTP server requests.")
                        .init(),
                ),
            );
        }

        if self
            .router
            .attributes
            .http_server_request_body_size
            .is_enabled()
        {
            static_instruments.insert(
                HTTP_SERVER_REQUEST_BODY_SIZE_METRIC.to_string(),
                StaticInstrument::Histogram(
                    meter
                        .f64_histogram(HTTP_SERVER_REQUEST_BODY_SIZE_METRIC)
                        .with_unit(Unit::new("By"))
                        .with_description("Size of HTTP server request bodies.")
                        .init(),
                ),
            );
        }

        if self
            .router
            .attributes
            .http_server_response_body_size
            .is_enabled()
        {
            static_instruments.insert(
                HTTP_SERVER_RESPONSE_BODY_SIZE_METRIC.to_string(),
                StaticInstrument::Histogram(
                    meter
                        .f64_histogram(HTTP_SERVER_RESPONSE_BODY_SIZE_METRIC)
                        .with_unit(Unit::new("By"))
                        .with_description("Size of HTTP server response bodies.")
                        .init(),
                ),
            );
        }

        if self
            .router
            .attributes
            .http_server_active_requests
            .is_enabled()
        {
            static_instruments.insert(
                HTTP_SERVER_ACTIVE_REQUESTS.to_string(),
                StaticInstrument::UpDownCounterI64(
                    meter
                        .i64_up_down_counter(HTTP_SERVER_ACTIVE_REQUESTS)
                        .with_unit(Unit::new("request"))
                        .with_description("Number of active HTTP server requests.")
                        .init(),
                ),
            );
        }

        for (instrument_name, instrument) in &self.router.custom {
            match instrument.ty {
                InstrumentType::Counter => {
                    static_instruments.insert(
                        instrument_name.clone(),
                        StaticInstrument::CounterF64(
                            meter
                                .f64_counter(instrument_name.clone())
                                .with_description(instrument.description.clone())
                                .with_unit(Unit::new(instrument.unit.clone()))
                                .init(),
                        ),
                    );
                }
                InstrumentType::Histogram => {
                    static_instruments.insert(
                        instrument_name.clone(),
                        StaticInstrument::Histogram(
                            meter
                                .f64_histogram(instrument_name.clone())
                                .with_description(instrument.description.clone())
                                .with_unit(Unit::new(instrument.unit.clone()))
                                .init(),
                        ),
                    );
                }
            }
        }

        static_instruments
    }

    pub(crate) fn new_router_instruments(
        &self,
        static_instruments: Arc<HashMap<String, StaticInstrument>>,
    ) -> RouterInstruments {
        let http_server_request_duration = self
            .router
            .attributes
            .http_server_request_duration
            .is_enabled()
            .then(|| CustomHistogram {
                inner: Mutex::new(CustomHistogramInner {
                    increment: Increment::Duration(Instant::now()),
                    condition: Condition::True,
                    histogram: Some(
                        static_instruments
                            .get(HTTP_SERVER_REQUEST_DURATION_METRIC)
                            .expect(
                                "cannot get static instrument for router; this should not happen",
                            )
                            .as_histogram()
                            .cloned()
                            .expect(
                                "cannot convert instrument to histogram for router; this should not happen",
                            ),
                    ),
                    attributes: Vec::new(),
                    selector: None,
                    selectors: match &self.router.attributes.http_server_request_duration {
                        DefaultedStandardInstrument::Bool(_)
                        | DefaultedStandardInstrument::Unset => None,
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            Some(attributes.clone())
                        }
                    },
                    updated: false,
                }),
            });
        let http_server_request_body_size =
            self.router
                .attributes
                .http_server_request_body_size
                .is_enabled()
                .then(|| {
                    let mut nb_attributes = 0;
                    let selectors = match &self.router.attributes.http_server_request_body_size {
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
                            histogram: Some(
                                static_instruments
                                    .get(HTTP_SERVER_REQUEST_BODY_SIZE_METRIC)
                                    .expect(
                                        "cannot get static instrument for router; this should not happen",
                                    )
                                    .as_histogram()
                                    .cloned().expect(
                                "cannot convert instrument to histogram for router; this should not happen",
                            )
                            ),
                            attributes: Vec::with_capacity(nb_attributes),
                            selector: Some(Arc::new(RouterSelector::RequestHeader {
                                request_header: "content-length".to_string(),
                                redact: None,
                                default: None,
                            })),
                            selectors,
                            updated: false,
                        }),
                    }
                });
        let http_server_response_body_size =
            self.router
                .attributes
                .http_server_response_body_size
                .is_enabled()
                .then(|| {
                    let mut nb_attributes = 0;
                    let selectors = match &self.router.attributes.http_server_response_body_size {
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
                            histogram: Some(
                                static_instruments
                                    .get(HTTP_SERVER_RESPONSE_BODY_SIZE_METRIC)
                                    .expect(
                                        "cannot get static instrument for router; this should not happen",
                                    )
                                    .as_histogram()
                                    .cloned()
                                    .expect(
                                    "cannot convert instrument to histogram for router; this should not happen",
                                    )
                            ),
                            attributes: Vec::with_capacity(nb_attributes),
                            selector: Some(Arc::new(RouterSelector::ResponseHeader {
                                response_header: "content-length".to_string(),
                                redact: None,
                                default: None,
                            })),
                            selectors,
                            updated: false,
                        }),
                    }
                });
        let http_server_active_requests = self
            .router
            .attributes
            .http_server_active_requests
            .is_enabled()
            .then(|| ActiveRequestsCounter {
                inner: Mutex::new(ActiveRequestsCounterInner {
                    counter: Some(
                        static_instruments
                            .get(HTTP_SERVER_ACTIVE_REQUESTS)
                            .expect(
                                "cannot get static instrument for router; this should not happen",
                            )
                            .as_up_down_counter_i64()
                            .cloned()
                            .expect(
                                "cannot convert instrument to up and down counter for router; this should not happen",
                            ),
                    ),
                    attrs_config: match &self.router.attributes.http_server_active_requests {
                        DefaultedStandardInstrument::Bool(_)
                        | DefaultedStandardInstrument::Unset => Default::default(),
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            attributes.clone()
                        }
                    },
                    attributes: Vec::new(),
                }),
            });
        RouterInstruments {
            http_server_request_duration,
            http_server_request_body_size,
            http_server_response_body_size,
            http_server_active_requests,
            custom: CustomInstruments::new(&self.router.custom, static_instruments),
        }
    }

    pub(crate) fn new_builtin_supergraph_instruments(&self) -> HashMap<String, StaticInstrument> {
        let meter = metrics::meter_provider().meter(METER_NAME);

        let mut static_instruments = HashMap::with_capacity(self.supergraph.custom.len());
        for (instrument_name, instrument) in &self.supergraph.custom {
            match instrument.ty {
                InstrumentType::Counter => {
                    static_instruments.insert(
                        instrument_name.clone(),
                        StaticInstrument::CounterF64(
                            meter
                                .f64_counter(instrument_name.clone())
                                .with_description(instrument.description.clone())
                                .with_unit(Unit::new(instrument.unit.clone()))
                                .init(),
                        ),
                    );
                }
                InstrumentType::Histogram => {
                    static_instruments.insert(
                        instrument_name.clone(),
                        StaticInstrument::Histogram(
                            meter
                                .f64_histogram(instrument_name.clone())
                                .with_description(instrument.description.clone())
                                .with_unit(Unit::new(instrument.unit.clone()))
                                .init(),
                        ),
                    );
                }
            }
        }
        static_instruments.extend(self.supergraph.attributes.cost.new_static_instruments());

        static_instruments
    }

    pub(crate) fn new_supergraph_instruments(
        &self,
        static_instruments: Arc<HashMap<String, StaticInstrument>>,
    ) -> SupergraphInstruments {
        SupergraphInstruments {
            cost: self
                .supergraph
                .attributes
                .cost
                .to_instruments(static_instruments.clone()),
            custom: CustomInstruments::new(&self.supergraph.custom, static_instruments),
        }
    }

    pub(crate) fn new_builtin_subgraph_instruments(&self) -> HashMap<String, StaticInstrument> {
        let meter = metrics::meter_provider().meter(METER_NAME);
        let mut static_instruments = HashMap::with_capacity(self.subgraph.custom.len());

        if self
            .subgraph
            .attributes
            .http_client_request_duration
            .is_enabled()
        {
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

        if self
            .subgraph
            .attributes
            .http_client_request_body_size
            .is_enabled()
        {
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

        if self
            .subgraph
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

        for (instrument_name, instrument) in &self.subgraph.custom {
            match instrument.ty {
                InstrumentType::Counter => {
                    static_instruments.insert(
                        instrument_name.clone(),
                        StaticInstrument::CounterF64(
                            meter
                                .f64_counter(instrument_name.clone())
                                .with_description(instrument.description.clone())
                                .with_unit(Unit::new(instrument.unit.clone()))
                                .init(),
                        ),
                    );
                }
                InstrumentType::Histogram => {
                    static_instruments.insert(
                        instrument_name.clone(),
                        StaticInstrument::Histogram(
                            meter
                                .f64_histogram(instrument_name.clone())
                                .with_description(instrument.description.clone())
                                .with_unit(Unit::new(instrument.unit.clone()))
                                .init(),
                        ),
                    );
                }
            }
        }

        static_instruments
    }

    pub(crate) fn new_subgraph_instruments(
        &self,
        static_instruments: Arc<HashMap<String, StaticInstrument>>,
    ) -> SubgraphInstruments {
        let http_client_request_duration =
            self.subgraph
                .attributes
                .http_client_request_duration
                .is_enabled()
                .then(|| {
                    let mut nb_attributes = 0;
                    let selectors = match &self.subgraph.attributes.http_client_request_duration {
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
                                    "cannot get static instrument for subgraph; this should not happen",
                                )
                                .as_histogram()
                                .cloned()
                                .expect(
                                    "cannot convert instrument to histogram for subgraph; this should not happen",
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
            self.subgraph
                .attributes
                .http_client_request_body_size
                .is_enabled()
                .then(|| {
                    let mut nb_attributes = 0;
                    let selectors = match &self.subgraph.attributes.http_client_request_body_size {
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
                                    "cannot get static instrument for subgraph; this should not happen",
                                )
                                .as_histogram()
                                .cloned()
                                .expect(
                                    "cannot convert instrument to histogram for subgraph; this should not happen",
                                )
                            ),
                            attributes: Vec::with_capacity(nb_attributes),
                            selector: Some(Arc::new(SubgraphSelector::SubgraphRequestHeader {
                                subgraph_request_header: "content-length".to_string(),
                                redact: None,
                                default: None,
                            })),
                            selectors,
                            updated: false,
                        }),
                    }
                });
        let http_client_response_body_size =
            self.subgraph
                .attributes
                .http_client_response_body_size
                .is_enabled()
                .then(|| {
                    let mut nb_attributes = 0;
                    let selectors = match &self.subgraph.attributes.http_client_response_body_size {
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
                                    "cannot get static instrument for subgraph; this should not happen",
                                )
                                .as_histogram()
                                .cloned()
                                .expect(
                                    "cannot convert instrument to histogram for subgraph; this should not happen",
                                )
                            ),
                            attributes: Vec::with_capacity(nb_attributes),
                            selector: Some(Arc::new(SubgraphSelector::SubgraphResponseHeader {
                                subgraph_response_header: "content-length".to_string(),
                                redact: None,
                                default: None,
                            })),
                            selectors,
                            updated: false,
                        }),
                    }
                });
        SubgraphInstruments {
            http_client_request_duration,
            http_client_request_body_size,
            http_client_response_body_size,
            custom: CustomInstruments::new(&self.subgraph.custom, static_instruments),
        }
    }

    pub(crate) fn new_builtin_graphql_instruments(&self) -> HashMap<String, StaticInstrument> {
        let meter = metrics::meter_provider().meter(METER_NAME);
        let mut static_instruments = HashMap::with_capacity(self.graphql.custom.len());
        if self.graphql.attributes.list_length.is_enabled() {
            static_instruments.insert(
                FIELD_LENGTH.to_string(),
                StaticInstrument::Histogram(
                    meter
                        .f64_histogram(FIELD_LENGTH)
                        .with_description("Length of a selected field in the GraphQL response")
                        .init(),
                ),
            );
        }

        if self.graphql.attributes.field_execution.is_enabled() {
            static_instruments.insert(
                FIELD_EXECUTION.to_string(),
                StaticInstrument::CounterF64(
                    meter
                        .f64_counter(FIELD_EXECUTION)
                        .with_description("Number of times a field is used.")
                        .init(),
                ),
            );
        }

        for (instrument_name, instrument) in &self.graphql.custom {
            match instrument.ty {
                InstrumentType::Counter => {
                    static_instruments.insert(
                        instrument_name.clone(),
                        StaticInstrument::CounterF64(
                            meter
                                .f64_counter(instrument_name.clone())
                                .with_description(instrument.description.clone())
                                .with_unit(Unit::new(instrument.unit.clone()))
                                .init(),
                        ),
                    );
                }
                InstrumentType::Histogram => {
                    static_instruments.insert(
                        instrument_name.clone(),
                        StaticInstrument::Histogram(
                            meter
                                .f64_histogram(instrument_name.clone())
                                .with_description(instrument.description.clone())
                                .with_unit(Unit::new(instrument.unit.clone()))
                                .init(),
                        ),
                    );
                }
            }
        }

        static_instruments
    }

    pub(crate) fn new_graphql_instruments(
        &self,
        static_instruments: Arc<HashMap<String, StaticInstrument>>,
    ) -> GraphQLInstruments {
        GraphQLInstruments {
            list_length: self.graphql.attributes.list_length.is_enabled().then(|| {
                let mut nb_attributes = 0;
                let selectors = match &self.graphql.attributes.list_length {
                    DefaultedStandardInstrument::Bool(_) | DefaultedStandardInstrument::Unset => {
                        None
                    }
                    DefaultedStandardInstrument::Extendable { attributes } => {
                        nb_attributes = attributes.custom.len();
                        Some(attributes.clone())
                    }
                };
                CustomHistogram {
                    inner: Mutex::new(CustomHistogramInner {
                        increment: Increment::FieldCustom(None),
                        condition: Condition::True,
                        histogram: Some(static_instruments
                                .get(FIELD_LENGTH)
                                .expect(
                                    "cannot get static instrument for graphql; this should not happen",
                                )
                                .as_histogram()
                                .cloned()
                                .expect(
                                    "cannot convert instrument to counter for graphql; this should not happen",
                                )
                            ),
                        attributes: Vec::with_capacity(nb_attributes),
                        selector: Some(Arc::new(GraphQLSelector::ListLength {
                            list_length: ListLength::Value,
                        })),
                        selectors,
                        updated: false,
                    }),
                }
            }),
            field_execution: self
                .graphql
                .attributes
                .field_execution
                .is_enabled()
                .then(|| {
                    let mut nb_attributes = 0;
                    let selectors = match &self.graphql.attributes.field_execution {
                        DefaultedStandardInstrument::Bool(_)
                        | DefaultedStandardInstrument::Unset => None,
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            nb_attributes = attributes.custom.len();
                            Some(attributes.clone())
                        }
                    };
                    CustomCounter {
                        inner: Mutex::new(CustomCounterInner {
                            increment: Increment::FieldUnit,
                            condition: Condition::True,
                            counter: Some(static_instruments
                                .get(FIELD_EXECUTION)
                                .expect(
                                    "cannot get static instrument for graphql; this should not happen",
                                )
                                .as_counter_f64()
                                .cloned()
                                .expect(
                                    "cannot convert instrument to counter for graphql; this should not happen",
                                )
                            ),
                            attributes: Vec::with_capacity(nb_attributes),
                            selector: None,
                            selectors,
                            incremented: false,
                        }),
                    }
                }),
            custom: CustomInstruments::new(&self.graphql.custom, static_instruments),
        }
    }

    pub(crate) fn new_builtin_cache_instruments(&self) -> HashMap<String, StaticInstrument> {
        let meter = metrics::meter_provider().meter(METER_NAME);
        let mut static_instruments: HashMap<String, StaticInstrument> = HashMap::new();
        if self.cache.attributes.cache.is_enabled() {
            static_instruments.insert(
                CACHE_METRIC.to_string(),
                StaticInstrument::CounterF64(
                    meter
                        .f64_counter(CACHE_METRIC)
                        .with_unit(Unit::new("ops"))
                        .with_description("Entity cache hit/miss operations at the subgraph level")
                        .init(),
                ),
            );
        }

        static_instruments
    }

    pub(crate) fn new_cache_instruments(
        &self,
        static_instruments: Arc<HashMap<String, StaticInstrument>>,
    ) -> CacheInstruments {
        CacheInstruments {
            cache_hit: self.cache.attributes.cache.is_enabled().then(|| {
                let mut nb_attributes = 0;
                let selectors = match &self.cache.attributes.cache {
                    DefaultedStandardInstrument::Bool(_) | DefaultedStandardInstrument::Unset => {
                        None
                    }
                    DefaultedStandardInstrument::Extendable { attributes } => {
                        nb_attributes = attributes.custom.len();
                        Some(attributes.clone())
                    }
                };
                CustomCounter {
                    inner: Mutex::new(CustomCounterInner {
                        increment: Increment::Custom(None),
                        condition: Condition::True,
                        counter: Some(static_instruments
                                .get(CACHE_METRIC)
                                .expect(
                                    "cannot get static instrument for cache; this should not happen",
                                )
                                .as_counter_f64()
                                .cloned()
                                .expect(
                                    "cannot convert instrument to counter for cache; this should not happen",
                                )
                            ),
                        attributes: Vec::with_capacity(nb_attributes),
                        selector: Some(Arc::new(SubgraphSelector::Cache {
                            cache: CacheKind::Hit,
                            entity_type: None,
                        })),
                        selectors,
                        incremented: false,
                    }),
                }
            }),
        }
    }
}

#[derive(Debug)]
pub(crate) enum StaticInstrument {
    CounterF64(Counter<f64>),
    UpDownCounterI64(UpDownCounter<i64>),
    Histogram(Histogram<f64>),
}

impl StaticInstrument {
    pub(crate) fn as_counter_f64(&self) -> Option<&Counter<f64>> {
        if let Self::CounterF64(v) = self {
            Some(v)
        } else {
            None
        }
    }

    pub(crate) fn as_up_down_counter_i64(&self) -> Option<&UpDownCounter<i64>> {
        if let Self::UpDownCounterI64(v) = self {
            Some(v)
        } else {
            None
        }
    }

    pub(crate) fn as_histogram(&self) -> Option<&Histogram<f64>> {
        if let Self::Histogram(v) = self {
            Some(v)
        } else {
            None
        }
    }
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct RouterInstrumentsConfig {
    /// Histogram of server request duration
    #[serde(rename = "http.server.request.duration")]
    http_server_request_duration:
        DefaultedStandardInstrument<Extendable<RouterAttributes, RouterSelector>>,

    /// Counter of active requests
    #[serde(rename = "http.server.active_requests")]
    http_server_active_requests: DefaultedStandardInstrument<ActiveRequestsAttributes>,

    /// Histogram of server request body size
    #[serde(rename = "http.server.request.body.size")]
    http_server_request_body_size:
        DefaultedStandardInstrument<Extendable<RouterAttributes, RouterSelector>>,

    /// Histogram of server response body size
    #[serde(rename = "http.server.response.body.size")]
    http_server_response_body_size:
        DefaultedStandardInstrument<Extendable<RouterAttributes, RouterSelector>>,
}

impl DefaultForLevel for RouterInstrumentsConfig {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        self.http_server_request_duration
            .defaults_for_levels(requirement_level, kind);
        self.http_server_active_requests
            .defaults_for_levels(requirement_level, kind);
        self.http_server_request_body_size
            .defaults_for_levels(requirement_level, kind);
        self.http_server_response_body_size
            .defaults_for_levels(requirement_level, kind);
    }
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ActiveRequestsAttributes {
    /// The HTTP request method
    #[serde(rename = "http.request.method")]
    http_request_method: bool,
    /// The server address
    #[serde(rename = "server.address")]
    server_address: bool,
    /// The server port
    #[serde(rename = "server.port")]
    server_port: bool,
    /// The URL scheme
    #[serde(rename = "url.scheme")]
    url_scheme: bool,
}

impl DefaultForLevel for ActiveRequestsAttributes {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        _kind: TelemetryDataKind,
    ) {
        match requirement_level {
            DefaultAttributeRequirementLevel::Required => {
                self.http_request_method = true;
                self.url_scheme = true;
            }
            DefaultAttributeRequirementLevel::Recommended
            | DefaultAttributeRequirementLevel::None => {}
        }
    }
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum DefaultedStandardInstrument<T> {
    #[default]
    Unset,
    Bool(bool),
    Extendable {
        attributes: Arc<T>,
    },
}

impl<T> DefaultedStandardInstrument<T> {
    pub(crate) fn is_enabled(&self) -> bool {
        match self {
            Self::Unset => false,
            Self::Bool(enabled) => *enabled,
            Self::Extendable { .. } => true,
        }
    }
}

impl<T> DefaultForLevel for DefaultedStandardInstrument<T>
where
    T: DefaultForLevel + Clone + Default,
{
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        match self {
            DefaultedStandardInstrument::Bool(enabled) if *enabled => match requirement_level {
                DefaultAttributeRequirementLevel::None => {}
                DefaultAttributeRequirementLevel::Required
                | DefaultAttributeRequirementLevel::Recommended => {
                    let mut attrs = T::default();
                    attrs.defaults_for_levels(requirement_level, kind);
                    *self = Self::Extendable {
                        attributes: Arc::new(attrs),
                    }
                }
            },
            DefaultedStandardInstrument::Unset => match requirement_level {
                DefaultAttributeRequirementLevel::None => {}
                DefaultAttributeRequirementLevel::Required
                | DefaultAttributeRequirementLevel::Recommended => {
                    let mut attrs = T::default();
                    attrs.defaults_for_levels(requirement_level, kind);
                    *self = Self::Extendable {
                        attributes: Arc::new(attrs),
                    }
                }
            },
            DefaultedStandardInstrument::Extendable { attributes } => {
                Arc::make_mut(attributes).defaults_for_levels(requirement_level, kind);
            }
            _ => {}
        }
    }
}

impl<T, Request, Response, EventResponse> Selectors for DefaultedStandardInstrument<T>
where
    T: Selectors<Request = Request, Response = Response, EventResponse = EventResponse>,
{
    type Request = Request;
    type Response = Response;
    type EventResponse = EventResponse;

    fn on_request(&self, request: &Self::Request) -> Vec<opentelemetry_api::KeyValue> {
        match self {
            Self::Bool(_) | Self::Unset => Vec::with_capacity(0),
            Self::Extendable { attributes } => attributes.on_request(request),
        }
    }

    fn on_response(&self, response: &Self::Response) -> Vec<opentelemetry_api::KeyValue> {
        match self {
            Self::Bool(_) | Self::Unset => Vec::with_capacity(0),
            Self::Extendable { attributes } => attributes.on_response(response),
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) -> Vec<opentelemetry_api::KeyValue> {
        match self {
            Self::Bool(_) | Self::Unset => Vec::with_capacity(0),
            Self::Extendable { attributes } => attributes.on_error(error, ctx),
        }
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) -> Vec<KeyValue> {
        match self {
            Self::Bool(_) | Self::Unset => Vec::with_capacity(0),
            Self::Extendable { attributes } => attributes.on_response_event(response, ctx),
        }
    }
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SupergraphInstrumentsConfig {
    #[serde(flatten)]
    pub(crate) cost: CostInstrumentsConfig,
}

impl DefaultForLevel for SupergraphInstrumentsConfig {
    fn defaults_for_level(
        &mut self,
        _requirement_level: DefaultAttributeRequirementLevel,
        _kind: TelemetryDataKind,
    ) {
    }
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubgraphInstrumentsConfig {
    /// Histogram of client request duration
    #[serde(rename = "http.client.request.duration")]
    http_client_request_duration:
        DefaultedStandardInstrument<Extendable<SubgraphAttributes, SubgraphSelector>>,

    /// Histogram of client request body size
    #[serde(rename = "http.client.request.body.size")]
    http_client_request_body_size:
        DefaultedStandardInstrument<Extendable<SubgraphAttributes, SubgraphSelector>>,

    /// Histogram of client response body size
    #[serde(rename = "http.client.response.body.size")]
    http_client_response_body_size:
        DefaultedStandardInstrument<Extendable<SubgraphAttributes, SubgraphSelector>>,
}

impl DefaultForLevel for SubgraphInstrumentsConfig {
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

#[derive(Clone, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct Instrument<A, E, V>
where
    A: Default + Debug,
    E: Debug,
    for<'a> &'a V: Into<InstrumentValue<E>>,
{
    /// The type of instrument.
    #[serde(rename = "type")]
    ty: InstrumentType,

    /// The value of the instrument.
    value: V,

    /// The description of the instrument.
    description: String,

    /// The units of the instrument, e.g. "ms", "bytes", "requests".
    unit: String,

    /// Attributes to include on the instrument.
    #[serde(default = "Extendable::empty_arc::<A, E>")]
    attributes: Arc<Extendable<A, E>>,

    /// The instrument conditions.
    #[serde(default = "Condition::empty::<E>")]
    condition: Condition<E>,
}

impl<A, E, Request, Response, EventResponse, SelectorValue> Selectors
    for Instrument<A, E, SelectorValue>
where
    A: Debug
        + Default
        + Selectors<Request = Request, Response = Response, EventResponse = EventResponse>,
    E: Debug + Selector<Request = Request, Response = Response, EventResponse = EventResponse>,
    for<'a> &'a SelectorValue: Into<InstrumentValue<E>>,
{
    type Request = Request;
    type Response = Response;
    type EventResponse = EventResponse;

    fn on_request(&self, request: &Self::Request) -> Vec<opentelemetry_api::KeyValue> {
        self.attributes.on_request(request)
    }

    fn on_response(&self, response: &Self::Response) -> Vec<opentelemetry_api::KeyValue> {
        self.attributes.on_response(response)
    }

    fn on_response_event(
        &self,
        response: &Self::EventResponse,
        ctx: &Context,
    ) -> Vec<opentelemetry_api::KeyValue> {
        self.attributes.on_response_event(response, ctx)
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) -> Vec<opentelemetry_api::KeyValue> {
        self.attributes.on_error(error, ctx)
    }
}

#[derive(Clone, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum InstrumentType {
    /// A monotonic counter https://opentelemetry.io/docs/specs/otel/metrics/data-model/#sums
    Counter,

    // /// A counter https://opentelemetry.io/docs/specs/otel/metrics/data-model/#sums
    // UpDownCounter,
    /// A histogram https://opentelemetry.io/docs/specs/otel/metrics/data-model/#histogram
    Histogram,
    // /// A gauge https://opentelemetry.io/docs/specs/otel/metrics/data-model/#gauge
    // Gauge,
}

#[derive(Clone, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub(crate) enum InstrumentValue<T> {
    Standard(Standard),
    Chunked(Event<T>),
    Field(Field<T>),
    Custom(T),
}

#[derive(Clone, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum StandardUnit {
    Unit,
}

#[derive(Clone, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Standard {
    Duration,
    Unit,
    // Active,
}

#[derive(Clone, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Event<T> {
    /// For every supergraph response payload (including subscription events and defer events)
    #[serde(rename = "event_duration")]
    Duration,
    /// For every supergraph response payload (including subscription events and defer events)
    #[serde(rename = "event_unit")]
    Unit,
    /// For every supergraph response payload (including subscription events and defer events)
    #[serde(rename = "event_custom")]
    Custom(T),
}

#[derive(Clone, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Field<T> {
    #[serde(rename = "field_unit")]
    Unit,
    /// For every field
    #[serde(rename = "field_custom")]
    Custom(T),
}

pub(crate) trait Instrumented {
    type Request;
    type Response;
    type EventResponse;

    fn on_request(&self, request: &Self::Request);
    fn on_response(&self, response: &Self::Response);
    fn on_response_event(&self, _response: &Self::EventResponse, _ctx: &Context) {}
    fn on_response_field(
        &self,
        _type: &apollo_compiler::executable::NamedType,
        _field: &apollo_compiler::executable::Field,
        _value: &Value,
        _ctx: &Context,
    ) {
    }
    fn on_error(&self, error: &BoxError, ctx: &Context);
}

impl<A, B, E, Request, Response, EventResponse, SelectorValue> Instrumented
    for Extendable<A, Instrument<B, E, SelectorValue>>
where
    A: Default
        + Instrumented<Request = Request, Response = Response, EventResponse = EventResponse>,
    B: Default
        + Debug
        + Selectors<Request = Request, Response = Response, EventResponse = EventResponse>,
    E: Debug + Selector<Request = Request, Response = Response, EventResponse = EventResponse>,
    for<'a> InstrumentValue<E>: From<&'a SelectorValue>,
{
    type Request = Request;
    type Response = Response;
    type EventResponse = EventResponse;

    fn on_request(&self, request: &Self::Request) {
        self.attributes.on_request(request);
    }

    fn on_response(&self, response: &Self::Response) {
        self.attributes.on_response(response);
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        self.attributes.on_response_event(response, ctx);
    }

    fn on_response_field(
        &self,
        ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &Value,
        ctx: &Context,
    ) {
        self.attributes.on_response_field(ty, field, value, ctx);
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        self.attributes.on_error(error, ctx);
    }
}

impl Selectors for SubgraphInstrumentsConfig {
    type Request = subgraph::Request;
    type Response = subgraph::Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) -> Vec<opentelemetry_api::KeyValue> {
        let mut attrs = self.http_client_request_body_size.on_request(request);
        attrs.extend(self.http_client_request_duration.on_request(request));
        attrs.extend(self.http_client_response_body_size.on_request(request));

        attrs
    }

    fn on_response(&self, response: &Self::Response) -> Vec<opentelemetry_api::KeyValue> {
        let mut attrs = self.http_client_request_body_size.on_response(response);
        attrs.extend(self.http_client_request_duration.on_response(response));
        attrs.extend(self.http_client_response_body_size.on_response(response));

        attrs
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) -> Vec<opentelemetry_api::KeyValue> {
        let mut attrs = self.http_client_request_body_size.on_error(error, ctx);
        attrs.extend(self.http_client_request_duration.on_error(error, ctx));
        attrs.extend(self.http_client_response_body_size.on_error(error, ctx));

        attrs
    }
}

pub(crate) struct CustomInstruments<Request, Response, Attributes, Select, SelectorValue>
where
    Attributes: Selectors<Request = Request, Response = Response> + Default,
    Select: Selector<Request = Request, Response = Response> + Debug,
{
    _phantom: PhantomData<SelectorValue>,
    counters: Vec<CustomCounter<Request, Response, Attributes, Select>>,
    histograms: Vec<CustomHistogram<Request, Response, Attributes, Select>>,
}

impl<Request, Response, Attributes, Select, SelectorValue>
    CustomInstruments<Request, Response, Attributes, Select, SelectorValue>
where
    Attributes: Selectors<Request = Request, Response = Response> + Default,
    Select: Selector<Request = Request, Response = Response> + Debug,
{
    pub(crate) fn is_empty(&self) -> bool {
        self.counters.is_empty() && self.histograms.is_empty()
    }
}

impl<Request, Response, Attributes, Select, SelectorValue>
    CustomInstruments<Request, Response, Attributes, Select, SelectorValue>
where
    Attributes: Selectors<Request = Request, Response = Response> + Default + Debug + Clone,
    Select: Selector<Request = Request, Response = Response> + Debug + Clone,
    for<'a> &'a SelectorValue: Into<InstrumentValue<Select>>,
{
    pub(crate) fn new(
        config: &HashMap<String, Instrument<Attributes, Select, SelectorValue>>,
        static_instruments: Arc<HashMap<String, StaticInstrument>>,
    ) -> Self {
        let mut counters = Vec::new();
        let mut histograms = Vec::new();

        for (instrument_name, instrument) in config {
            match instrument.ty {
                InstrumentType::Counter => {
                    let (selector, increment) = match (&instrument.value).into() {
                        InstrumentValue::Standard(incr) => {
                            let incr = match incr {
                                Standard::Duration => Increment::Duration(Instant::now()),
                                Standard::Unit => Increment::Unit,
                            };
                            (None, incr)
                        }
                        InstrumentValue::Custom(selector) => {
                            (Some(Arc::new(selector)), Increment::Custom(None))
                        }
                        InstrumentValue::Chunked(incr) => match incr {
                            Event::Duration => (None, Increment::EventDuration(Instant::now())),
                            Event::Unit => (None, Increment::EventUnit),
                            Event::Custom(selector) => {
                                (Some(Arc::new(selector)), Increment::EventCustom(None))
                            }
                        },
                        InstrumentValue::Field(incr) => match incr {
                            Field::Unit => (None, Increment::FieldUnit),
                            Field::Custom(selector) => {
                                (Some(Arc::new(selector)), Increment::FieldCustom(None))
                            }
                        },
                    };
                    match static_instruments
                        .get(instrument_name)
                        .expect(
                            "cannot get static instrument for supergraph; this should not happen",
                        )
                        .as_counter_f64()
                        .cloned()
                    {
                        Some(counter) => {
                            let counter = CustomCounterInner {
                                increment,
                                condition: instrument.condition.clone(),
                                counter: Some(counter),
                                attributes: Vec::new(),
                                selector,
                                selectors: Some(instrument.attributes.clone()),
                                incremented: false,
                            };
                            counters.push(CustomCounter {
                                inner: Mutex::new(counter),
                            })
                        }
                        None => {
                            ::tracing::error!("cannot convert static instrument into a counter, this is an error; please fill an issue on GitHub");
                        }
                    }
                }
                InstrumentType::Histogram => {
                    let (selector, increment) = match (&instrument.value).into() {
                        InstrumentValue::Standard(incr) => {
                            let incr = match incr {
                                Standard::Duration => Increment::Duration(Instant::now()),
                                Standard::Unit => Increment::Unit,
                            };
                            (None, incr)
                        }
                        InstrumentValue::Custom(selector) => {
                            (Some(Arc::new(selector)), Increment::Custom(None))
                        }
                        InstrumentValue::Chunked(incr) => match incr {
                            Event::Duration => (None, Increment::EventDuration(Instant::now())),
                            Event::Unit => (None, Increment::EventUnit),
                            Event::Custom(selector) => {
                                (Some(Arc::new(selector)), Increment::EventCustom(None))
                            }
                        },
                        InstrumentValue::Field(incr) => match incr {
                            Field::Unit => (None, Increment::FieldUnit),
                            Field::Custom(selector) => {
                                (Some(Arc::new(selector)), Increment::FieldCustom(None))
                            }
                        },
                    };

                    match static_instruments
                        .get(instrument_name)
                        .expect(
                            "cannot get static instrument for supergraph; this should not happen",
                        )
                        .as_histogram()
                        .cloned()
                    {
                        Some(histogram) => {
                            let histogram = CustomHistogramInner {
                                increment,
                                condition: instrument.condition.clone(),
                                histogram: Some(histogram),
                                attributes: Vec::new(),
                                selector,
                                selectors: Some(instrument.attributes.clone()),
                                updated: false,
                            };

                            histograms.push(CustomHistogram {
                                inner: Mutex::new(histogram),
                            });
                        }
                        None => {
                            ::tracing::error!("cannot convert static instrument into a histogram, this is an error; please fill an issue on GitHub");
                        }
                    }
                }
            }
        }

        Self {
            _phantom: Default::default(),
            counters,
            histograms,
        }
    }
}

impl<Request, Response, EventResponse, Attributes, Select, SelectorValue> Instrumented
    for CustomInstruments<Request, Response, Attributes, Select, SelectorValue>
where
    Attributes:
        Selectors<Request = Request, Response = Response, EventResponse = EventResponse> + Default,
    Select: Selector<Request = Request, Response = Response, EventResponse = EventResponse> + Debug,
{
    type Request = Request;
    type Response = Response;
    type EventResponse = EventResponse;

    fn on_request(&self, request: &Self::Request) {
        for counter in &self.counters {
            counter.on_request(request);
        }
        for histogram in &self.histograms {
            histogram.on_request(request);
        }
    }

    fn on_response(&self, response: &Self::Response) {
        for counter in &self.counters {
            counter.on_response(response);
        }
        for histogram in &self.histograms {
            histogram.on_response(response);
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        for counter in &self.counters {
            counter.on_error(error, ctx);
        }
        for histogram in &self.histograms {
            histogram.on_error(error, ctx);
        }
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        for counter in &self.counters {
            counter.on_response_event(response, ctx);
        }
        for histogram in &self.histograms {
            histogram.on_response_event(response, ctx);
        }
    }

    fn on_response_field(
        &self,
        ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &Value,
        ctx: &Context,
    ) {
        for counter in &self.counters {
            counter.on_response_field(ty, field, value, ctx);
        }
        for histogram in &self.histograms {
            histogram.on_response_field(ty, field, value, ctx);
        }
    }
}

pub(crate) struct RouterInstruments {
    http_server_request_duration: Option<
        CustomHistogram<router::Request, router::Response, RouterAttributes, RouterSelector>,
    >,
    http_server_active_requests: Option<ActiveRequestsCounter>,
    http_server_request_body_size: Option<
        CustomHistogram<router::Request, router::Response, RouterAttributes, RouterSelector>,
    >,
    http_server_response_body_size: Option<
        CustomHistogram<router::Request, router::Response, RouterAttributes, RouterSelector>,
    >,
    custom: RouterCustomInstruments,
}

impl Instrumented for RouterInstruments {
    type Request = router::Request;
    type Response = router::Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        if let Some(http_server_request_duration) = &self.http_server_request_duration {
            http_server_request_duration.on_request(request);
        }
        if let Some(http_server_active_requests) = &self.http_server_active_requests {
            http_server_active_requests.on_request(request);
        }
        if let Some(http_server_request_body_size) = &self.http_server_request_body_size {
            http_server_request_body_size.on_request(request);
        }
        if let Some(http_server_response_body_size) = &self.http_server_response_body_size {
            http_server_response_body_size.on_request(request);
        }
        self.custom.on_request(request);
    }

    fn on_response(&self, response: &Self::Response) {
        if let Some(http_server_request_duration) = &self.http_server_request_duration {
            http_server_request_duration.on_response(response);
        }
        if let Some(http_server_active_requests) = &self.http_server_active_requests {
            http_server_active_requests.on_response(response);
        }
        if let Some(http_server_request_body_size) = &self.http_server_request_body_size {
            http_server_request_body_size.on_response(response);
        }
        if let Some(http_server_response_body_size) = &self.http_server_response_body_size {
            http_server_response_body_size.on_response(response);
        }
        self.custom.on_response(response);
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        if let Some(http_server_request_duration) = &self.http_server_request_duration {
            http_server_request_duration.on_error(error, ctx);
        }
        if let Some(http_server_active_requests) = &self.http_server_active_requests {
            http_server_active_requests.on_error(error, ctx);
        }
        if let Some(http_server_request_body_size) = &self.http_server_request_body_size {
            http_server_request_body_size.on_error(error, ctx);
        }
        if let Some(http_server_response_body_size) = &self.http_server_response_body_size {
            http_server_response_body_size.on_error(error, ctx);
        }
        self.custom.on_error(error, ctx);
    }
}

pub(crate) struct SupergraphInstruments {
    cost: CostInstruments,
    custom: SupergraphCustomInstruments,
}

impl Instrumented for SupergraphInstruments {
    type Request = supergraph::Request;
    type Response = supergraph::Response;
    type EventResponse = crate::graphql::Response;

    fn on_request(&self, request: &Self::Request) {
        self.cost.on_request(request);
        self.custom.on_request(request);
    }

    fn on_response(&self, response: &Self::Response) {
        self.cost.on_response(response);
        self.custom.on_response(response);
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        self.cost.on_error(error, ctx);
        self.custom.on_error(error, ctx);
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        self.cost.on_response_event(response, ctx);
        self.custom.on_response_event(response, ctx);
    }
}

pub(crate) struct SubgraphInstruments {
    http_client_request_duration: Option<
        CustomHistogram<
            subgraph::Request,
            subgraph::Response,
            SubgraphAttributes,
            SubgraphSelector,
        >,
    >,
    http_client_request_body_size: Option<
        CustomHistogram<
            subgraph::Request,
            subgraph::Response,
            SubgraphAttributes,
            SubgraphSelector,
        >,
    >,
    http_client_response_body_size: Option<
        CustomHistogram<
            subgraph::Request,
            subgraph::Response,
            SubgraphAttributes,
            SubgraphSelector,
        >,
    >,
    custom: SubgraphCustomInstruments,
}

impl Instrumented for SubgraphInstruments {
    type Request = subgraph::Request;
    type Response = subgraph::Response;
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

pub(crate) type RouterCustomInstruments = CustomInstruments<
    router::Request,
    router::Response,
    RouterAttributes,
    RouterSelector,
    RouterValue,
>;

pub(crate) type SupergraphCustomInstruments = CustomInstruments<
    supergraph::Request,
    supergraph::Response,
    SupergraphAttributes,
    SupergraphSelector,
    SupergraphValue,
>;

pub(crate) type SubgraphCustomInstruments = CustomInstruments<
    subgraph::Request,
    subgraph::Response,
    SubgraphAttributes,
    SubgraphSelector,
    SubgraphValue,
>;

// ---------------- Counter -----------------------
#[derive(Debug, Clone)]
pub(crate) enum Increment {
    Unit,
    EventUnit,
    FieldUnit,
    Duration(Instant),
    EventDuration(Instant),
    Custom(Option<i64>),
    EventCustom(Option<i64>),
    FieldCustom(Option<i64>),
}

fn to_i64(value: opentelemetry::Value) -> Option<i64> {
    match value {
        opentelemetry::Value::I64(i) => Some(i),
        opentelemetry::Value::String(s) => s.as_str().parse::<i64>().ok(),
        opentelemetry::Value::F64(f) => Some(f.floor() as i64),
        opentelemetry::Value::Bool(_) => None,
        opentelemetry::Value::Array(_) => None,
    }
}

pub(crate) struct CustomCounter<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response> + Debug,
{
    pub(crate) inner: Mutex<CustomCounterInner<Request, Response, A, T>>,
}

impl<Request, Response, A, T> Clone for CustomCounter<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response> + Debug + Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: Mutex::new(self.inner.lock().clone()),
        }
    }
}

pub(crate) struct CustomCounterInner<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response> + Debug,
{
    pub(crate) increment: Increment,
    pub(crate) selector: Option<Arc<T>>,
    pub(crate) selectors: Option<Arc<Extendable<A, T>>>,
    pub(crate) counter: Option<Counter<f64>>,
    pub(crate) condition: Condition<T>,
    pub(crate) attributes: Vec<opentelemetry_api::KeyValue>,
    // Useful when it's a counter on events to know if we have to count for an event or not
    pub(crate) incremented: bool,
}

impl<Request, Response, A, T> Clone for CustomCounterInner<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response> + Debug + Clone,
{
    fn clone(&self) -> Self {
        Self {
            increment: self.increment.clone(),
            selector: self.selector.clone(),
            selectors: self.selectors.clone(),
            counter: self.counter.clone(),
            condition: self.condition.clone(),
            attributes: self.attributes.clone(),
            incremented: self.incremented,
        }
    }
}

impl<A, T, Request, Response, EventResponse> Instrumented for CustomCounter<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response, EventResponse = EventResponse> + Default,
    T: Selector<Request = Request, Response = Response, EventResponse = EventResponse>
        + Debug
        + Debug,
{
    type Request = Request;
    type Response = Response;
    type EventResponse = EventResponse;

    fn on_request(&self, request: &Self::Request) {
        let mut inner = self.inner.lock();
        if inner.condition.evaluate_request(request) == Some(false) {
            let _ = inner.counter.take();
            return;
        }
        if let Some(selectors) = inner.selectors.as_ref() {
            inner.attributes = selectors.on_request(request).into_iter().collect();
        }

        if let Some(selected_value) = inner.selector.as_ref().and_then(|s| s.on_request(request)) {
            let new_incr = match &inner.increment {
                Increment::EventCustom(None) => Increment::EventCustom(to_i64(selected_value)),
                Increment::Custom(None) => Increment::Custom(to_i64(selected_value)),
                other => {
                    failfast_error!("this is a bug and should not happen, the increment should only be Custom or EventCustom, please open an issue: {other:?}");
                    return;
                }
            };
            inner.increment = new_incr;
        }
    }

    fn on_response(&self, response: &Self::Response) {
        let mut inner = self.inner.lock();
        if !inner.condition.evaluate_response(response) {
            if !matches!(
                &inner.increment,
                Increment::EventCustom(_)
                    | Increment::EventDuration(_)
                    | Increment::EventUnit
                    | Increment::FieldCustom(_)
                    | Increment::FieldUnit
            ) {
                let _ = inner.counter.take();
            }
            return;
        }

        let attrs: Vec<KeyValue> = inner
            .selectors
            .as_ref()
            .map(|s| s.on_response(response).into_iter().collect())
            .unwrap_or_default();
        inner.attributes.extend(attrs);

        if let Some(selected_value) = inner
            .selector
            .as_ref()
            .and_then(|s| s.on_response(response))
        {
            let new_incr = match &inner.increment {
                Increment::EventCustom(None) => Increment::Custom(to_i64(selected_value)),
                Increment::Custom(None) => Increment::Custom(to_i64(selected_value)),
                other => {
                    failfast_error!("this is a bug and should not happen, the increment should only be Custom or EventCustom, please open an issue: {other:?}");
                    return;
                }
            };
            inner.increment = new_incr;
        }

        let increment = match inner.increment {
            Increment::Unit => 1f64,
            Increment::Duration(instant) => instant.elapsed().as_secs_f64(),
            Increment::Custom(val) => match val {
                Some(incr) => incr as f64,
                None => 0f64,
            },
            Increment::EventUnit
            | Increment::EventDuration(_)
            | Increment::EventCustom(_)
            | Increment::FieldUnit
            | Increment::FieldCustom(_) => {
                // Nothing to do because we're incrementing on events or fields
                return;
            }
        };

        if increment != 0.0 {
            if let Some(counter) = &inner.counter {
                counter.add(increment, &inner.attributes);
            }
            inner.incremented = true;
        }
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        let mut inner = self.inner.lock();
        if !inner.condition.evaluate_event_response(response, ctx) {
            return;
        }
        // Response event may be called multiple times so we don't extend inner.attributes
        let mut attrs = inner.attributes.clone();
        if let Some(selectors) = inner.selectors.as_ref() {
            attrs.extend(
                selectors
                    .on_response_event(response, ctx)
                    .into_iter()
                    .collect::<Vec<_>>(),
            );
        }

        if let Some(selected_value) = inner
            .selector
            .as_ref()
            .and_then(|s| s.on_response_event(response, ctx))
        {
            let new_incr = match &inner.increment {
                Increment::EventCustom(None) => Increment::EventCustom(to_i64(selected_value)),
                Increment::Custom(None) => Increment::EventCustom(to_i64(selected_value)),
                other => {
                    failfast_error!("this is a bug and should not happen, the increment should only be Custom or EventCustom, please open an issue: {other:?}");
                    return;
                }
            };
            inner.increment = new_incr;
        }

        let increment = match &mut inner.increment {
            Increment::EventUnit => 1f64,
            Increment::EventDuration(instant) => {
                let incr = instant.elapsed().as_secs_f64();
                // Set it to new instant for the next event
                *instant = Instant::now();
                incr
            }
            Increment::Custom(val) | Increment::EventCustom(val) => {
                let incr = match val {
                    Some(incr) => *incr as f64,
                    None => 0f64,
                };
                // Set it to None again for the next event
                *val = None;
                incr
            }
            _ => 0f64,
        };

        inner.incremented = true;
        if let Some(counter) = &inner.counter {
            counter.add(increment, &attrs);
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        let mut inner = self.inner.lock();

        let mut attrs = inner.attributes.clone();
        if let Some(selectors) = inner.selectors.as_ref() {
            attrs.extend(
                selectors
                    .on_error(error, ctx)
                    .into_iter()
                    .collect::<Vec<_>>(),
            );
        }

        let increment = match inner.increment {
            Increment::Unit | Increment::EventUnit | Increment::FieldUnit => 1f64,
            Increment::Duration(instant) | Increment::EventDuration(instant) => {
                instant.elapsed().as_secs_f64()
            }
            Increment::Custom(val) | Increment::EventCustom(val) | Increment::FieldCustom(val) => {
                match val {
                    Some(incr) => incr as f64,
                    None => 0f64,
                }
            }
        };

        if let Some(counter) = inner.counter.take() {
            counter.add(increment, &attrs);
        }
    }

    fn on_response_field(
        &self,
        ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &serde_json_bytes::Value,
        ctx: &Context,
    ) {
        let mut inner = self.inner.lock();
        if !inner
            .condition
            .evaluate_response_field(ty, field, value, ctx)
        {
            return;
        }

        if let Some(selected_value) = inner
            .selector
            .as_ref()
            .and_then(|s| s.on_response_field(ty, field, value, ctx))
        {
            let new_incr = match &inner.increment {
                Increment::FieldCustom(None) => Increment::FieldCustom(to_i64(selected_value)),
                Increment::Custom(None) => Increment::FieldCustom(to_i64(selected_value)),
                other => {
                    failfast_error!("this is a bug and should not happen, the increment should only be Custom or FieldCustom, please open an issue: {other:?}");
                    return;
                }
            };
            inner.increment = new_incr;
        }

        let increment: Option<f64> = match &mut inner.increment {
            Increment::FieldUnit => Some(1f64),
            Increment::FieldCustom(val) => {
                let incr = val.map(|incr| incr as f64);
                // Set it to None again
                *val = None;
                incr
            }
            Increment::Unit
            | Increment::Duration(_)
            | Increment::Custom(_)
            | Increment::EventDuration(_)
            | Increment::EventCustom(_)
            | Increment::EventUnit => {
                // Nothing to do because we're incrementing on fields
                return;
            }
        };

        // Response field may be called multiple times
        // But there's no need for us to create a new vec each time, we can just extend the existing one and then reset it after
        let original_length = inner.attributes.len();
        if inner.counter.is_some() && increment.is_some() {
            // Only get the attributes from the selectors if we are actually going to increment the histogram
            // Cloning selectors should not have to happen
            let selectors = inner.selectors.clone();
            let attributes = &mut inner.attributes;
            if let Some(selectors) = selectors {
                selectors.on_response_field(attributes, ty, field, value, ctx);
            }
        }

        if let (Some(counter), Some(increment)) = (&inner.counter, increment) {
            counter.add(increment, &inner.attributes);
            // Reset the attributes to the original length, this will discard the new attributes added from selectors.
            inner.attributes.truncate(original_length);
        }
    }
}

impl<A, T, Request, Response> Drop for CustomCounter<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response> + Debug,
{
    fn drop(&mut self) {
        // TODO add attribute error broken pipe ? cf https://github.com/apollographql/router/issues/4866
        let inner = self.inner.try_lock();
        if let Some(mut inner) = inner {
            // If the condition is false or indeterminate then we don't increment the counter
            if inner.incremented || matches!(inner.condition.evaluate_drop(), Some(false) | None) {
                return;
            }
            if let Some(counter) = inner.counter.take() {
                let incr: f64 = match &inner.increment {
                    Increment::Unit | Increment::EventUnit => 1f64,
                    Increment::Duration(instant) | Increment::EventDuration(instant) => {
                        instant.elapsed().as_secs_f64()
                    }
                    Increment::Custom(val) | Increment::EventCustom(val) => match val {
                        Some(incr) => *incr as f64,
                        None => 0f64,
                    },
                    Increment::FieldUnit | Increment::FieldCustom(_) => {
                        // Dropping a metric on a field will never increment.
                        // We can't increment graphql metrics unless we actually process the result.
                        // It's not like we're counting the number of requests, where we want to increment
                        // with the data that we know so far if the request stops.
                        return;
                    }
                };
                counter.add(incr, &inner.attributes);
            }
        }
    }
}

struct ActiveRequestsCounter {
    inner: Mutex<ActiveRequestsCounterInner>,
}

struct ActiveRequestsCounterInner {
    counter: Option<UpDownCounter<i64>>,
    attrs_config: Arc<ActiveRequestsAttributes>,
    attributes: Vec<opentelemetry_api::KeyValue>,
}

impl Instrumented for ActiveRequestsCounter {
    type Request = router::Request;
    type Response = router::Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        let mut inner = self.inner.lock();
        if inner.attrs_config.http_request_method {
            if let Some(attr) = (RouterSelector::RequestMethod {
                request_method: true,
            })
            .on_request(request)
            {
                inner
                    .attributes
                    .push(KeyValue::new(HTTP_REQUEST_METHOD, attr));
            }
        }
        if inner.attrs_config.server_address {
            if let Some(attr) = HttpServerAttributes::forwarded_host(request)
                .and_then(|h| h.host().map(|h| h.to_string()))
            {
                inner.attributes.push(KeyValue::new(SERVER_ADDRESS, attr));
            }
        }
        if inner.attrs_config.server_port {
            if let Some(attr) =
                HttpServerAttributes::forwarded_host(request).and_then(|h| h.port_u16())
            {
                inner
                    .attributes
                    .push(KeyValue::new(SERVER_PORT, attr as i64));
            }
        }
        if inner.attrs_config.url_scheme {
            if let Some(attr) = request.router_request.uri().scheme_str() {
                inner
                    .attributes
                    .push(KeyValue::new(URL_SCHEME, attr.to_string()));
            }
        }
        if let Some(counter) = &inner.counter {
            counter.add(1, &inner.attributes);
        }
    }

    fn on_response(&self, _response: &Self::Response) {
        let mut inner = self.inner.lock();
        if let Some(counter) = &inner.counter.take() {
            counter.add(-1, &inner.attributes);
        }
    }

    fn on_error(&self, _error: &BoxError, _ctx: &Context) {
        let mut inner = self.inner.lock();
        if let Some(counter) = &inner.counter.take() {
            counter.add(-1, &inner.attributes);
        }
    }
}

impl Drop for ActiveRequestsCounter {
    fn drop(&mut self) {
        let inner = self.inner.try_lock();
        if let Some(mut inner) = inner {
            if let Some(counter) = &inner.counter.take() {
                counter.add(-1, &inner.attributes);
            }
        }
    }
}

// ---------------- Histogram -----------------------

pub(crate) struct CustomHistogram<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response>,
{
    pub(crate) inner: Mutex<CustomHistogramInner<Request, Response, A, T>>,
}

pub(crate) struct CustomHistogramInner<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response>,
{
    pub(crate) increment: Increment,
    pub(crate) condition: Condition<T>,
    pub(crate) selector: Option<Arc<T>>,
    pub(crate) selectors: Option<Arc<Extendable<A, T>>>,
    pub(crate) histogram: Option<Histogram<f64>>,
    pub(crate) attributes: Vec<opentelemetry_api::KeyValue>,
    // Useful when it's an histogram on events to know if we have to count for an event or not
    pub(crate) updated: bool,
}

impl<A, T, Request, Response, EventResponse> Instrumented
    for CustomHistogram<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response, EventResponse = EventResponse> + Default,
    T: Selector<Request = Request, Response = Response, EventResponse = EventResponse>,
{
    type Request = Request;
    type Response = Response;
    type EventResponse = EventResponse;

    fn on_request(&self, request: &Self::Request) {
        let mut inner = self.inner.lock();
        if inner.condition.evaluate_request(request) == Some(false) {
            let _ = inner.histogram.take();
            return;
        }
        if let Some(selectors) = &inner.selectors {
            inner.attributes = selectors.on_request(request).into_iter().collect();
        }
        if let Some(selected_value) = inner.selector.as_ref().and_then(|s| s.on_request(request)) {
            let new_incr = match &inner.increment {
                Increment::EventCustom(None) => Increment::EventCustom(to_i64(selected_value)),
                Increment::FieldCustom(None) => Increment::FieldCustom(to_i64(selected_value)),
                Increment::Custom(None) => Increment::Custom(to_i64(selected_value)),
                other => {
                    failfast_error!("this is a bug and should not happen, the increment should only be Custom or EventCustom, please open an issue: {other:?}");
                    return;
                }
            };
            inner.increment = new_incr;
        }
    }

    fn on_response(&self, response: &Self::Response) {
        let mut inner = self.inner.lock();
        if !inner.condition.evaluate_response(response) {
            if !matches!(
                &inner.increment,
                Increment::EventCustom(_)
                    | Increment::EventDuration(_)
                    | Increment::EventUnit
                    | Increment::FieldCustom(_)
                    | Increment::FieldUnit
            ) {
                let _ = inner.histogram.take();
            }
            return;
        }
        let attrs: Vec<KeyValue> = inner
            .selectors
            .as_ref()
            .map(|s| s.on_response(response).into_iter().collect())
            .unwrap_or_default();
        inner.attributes.extend(attrs);
        if let Some(selected_value) = inner
            .selector
            .as_ref()
            .and_then(|s| s.on_response(response))
        {
            let new_incr = match &inner.increment {
                Increment::EventCustom(None) => Increment::EventCustom(to_i64(selected_value)),
                Increment::FieldCustom(None) => Increment::FieldCustom(to_i64(selected_value)),
                Increment::Custom(None) => Increment::Custom(to_i64(selected_value)),
                other => {
                    failfast_error!("this is a bug and should not happen, the increment should only be Custom or EventCustom, please open an issue: {other:?}");
                    return;
                }
            };
            inner.increment = new_incr;
        }

        let increment = match inner.increment {
            Increment::Unit => Some(1f64),
            Increment::Duration(instant) => Some(instant.elapsed().as_secs_f64()),
            Increment::Custom(val) => val.map(|incr| incr as f64),
            Increment::EventUnit
            | Increment::EventDuration(_)
            | Increment::EventCustom(_)
            | Increment::FieldUnit
            | Increment::FieldCustom(_) => {
                // Nothing to do because we're incrementing on events or fields
                return;
            }
        };

        if let (Some(histogram), Some(increment)) = (&inner.histogram, increment) {
            histogram.record(increment, &inner.attributes);
            inner.updated = true;
        }
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        let mut inner = self.inner.lock();
        if !inner.condition.evaluate_event_response(response, ctx) {
            return;
        }

        // Response event may be called multiple times so we don't extend inner.attributes
        let mut attrs: Vec<KeyValue> = inner.attributes.clone();
        if let Some(selectors) = inner.selectors.as_ref() {
            attrs.extend(
                selectors
                    .on_response_event(response, ctx)
                    .into_iter()
                    .collect::<Vec<_>>(),
            );
        }

        if let Some(selected_value) = inner
            .selector
            .as_ref()
            .and_then(|s| s.on_response_event(response, ctx))
        {
            let new_incr = match &inner.increment {
                Increment::EventCustom(None) => Increment::EventCustom(to_i64(selected_value)),
                Increment::Custom(None) => Increment::EventCustom(to_i64(selected_value)),
                other => {
                    failfast_error!("this is a bug and should not happen, the increment should only be Custom or EventCustom, please open an issue: {other:?}");
                    return;
                }
            };
            inner.increment = new_incr;
        }

        let increment: Option<f64> = match &mut inner.increment {
            Increment::EventUnit => Some(1f64),
            Increment::EventDuration(instant) => {
                let incr = Some(instant.elapsed().as_secs_f64());
                // Need a new instant for the next event
                *instant = Instant::now();
                incr
            }
            Increment::EventCustom(val) => {
                let incr = val.map(|incr| incr as f64);
                // Set it to None again
                *val = None;
                incr
            }
            Increment::Unit
            | Increment::Duration(_)
            | Increment::Custom(_)
            | Increment::FieldUnit
            | Increment::FieldCustom(_) => {
                // Nothing to do because we're incrementing on events
                return;
            }
        };
        if let (Some(histogram), Some(increment)) = (&inner.histogram, increment) {
            histogram.record(increment, &attrs);
            inner.updated = true;
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        let mut inner = self.inner.lock();
        let mut attrs: Vec<KeyValue> = inner
            .selectors
            .as_ref()
            .map(|s| s.on_error(error, ctx).into_iter().collect())
            .unwrap_or_default();
        attrs.append(&mut inner.attributes);

        let increment = match inner.increment {
            Increment::Unit | Increment::EventUnit | Increment::FieldUnit => Some(1f64),
            Increment::Duration(instant) | Increment::EventDuration(instant) => {
                Some(instant.elapsed().as_secs_f64())
            }
            Increment::Custom(val) | Increment::EventCustom(val) | Increment::FieldCustom(val) => {
                val.map(|incr| incr as f64)
            }
        };

        if let (Some(histogram), Some(increment)) = (inner.histogram.take(), increment) {
            histogram.record(increment, &attrs);
        }
    }

    fn on_response_field(
        &self,
        ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &serde_json_bytes::Value,
        ctx: &Context,
    ) {
        let mut inner = self.inner.lock();
        if !inner
            .condition
            .evaluate_response_field(ty, field, value, ctx)
        {
            return;
        }

        if let Some(selected_value) = inner
            .selector
            .as_ref()
            .and_then(|s| s.on_response_field(ty, field, value, ctx))
        {
            let new_incr = match &inner.increment {
                Increment::FieldCustom(None) => Increment::FieldCustom(to_i64(selected_value)),
                Increment::Custom(None) => Increment::FieldCustom(to_i64(selected_value)),
                other => {
                    failfast_error!("this is a bug and should not happen, the increment should only be Custom or FieldCustom, please open an issue: {other:?}");
                    return;
                }
            };
            inner.increment = new_incr;
        }

        let increment: Option<f64> = match &mut inner.increment {
            Increment::FieldUnit => Some(1f64),
            Increment::FieldCustom(val) => {
                let incr = val.map(|incr| incr as f64);
                // Set it to None again
                *val = None;
                incr
            }
            Increment::Unit
            | Increment::Duration(_)
            | Increment::Custom(_)
            | Increment::EventDuration(_)
            | Increment::EventCustom(_)
            | Increment::EventUnit => {
                // Nothing to do because we're incrementing on fields
                return;
            }
        };

        // Response field may be called multiple times
        // But there's no need for us to create a new vec each time, we can just extend the existing one and then reset it after
        let original_length = inner.attributes.len();
        if inner.histogram.is_some() && increment.is_some() {
            // Only get the attributes from the selectors if we are actually going to increment the histogram
            // Cloning selectors should not have to happen
            let selectors = inner.selectors.clone();
            let attributes = &mut inner.attributes;
            if let Some(selectors) = selectors {
                selectors.on_response_field(attributes, ty, field, value, ctx);
            }
        }

        if let (Some(histogram), Some(increment)) = (&inner.histogram, increment) {
            histogram.record(increment, &inner.attributes);
            // Reset the attributes to the original length, this will discard the new attributes added from selectors.
            inner.attributes.truncate(original_length);
        }
    }
}

impl<A, T, Request, Response> Drop for CustomHistogram<Request, Response, A, T>
where
    A: Selectors<Request = Request, Response = Response> + Default,
    T: Selector<Request = Request, Response = Response>,
{
    fn drop(&mut self) {
        // TODO add attribute error broken pipe ? cf https://github.com/apollographql/router/issues/4866
        let inner = self.inner.try_lock();
        if let Some(mut inner) = inner {
            if inner.updated || matches!(inner.condition.evaluate_drop(), Some(false) | None) {
                return;
            }
            if let Some(histogram) = inner.histogram.take() {
                let increment = match &inner.increment {
                    Increment::Unit | Increment::EventUnit => Some(1f64),
                    Increment::Duration(instant) | Increment::EventDuration(instant) => {
                        Some(instant.elapsed().as_secs_f64())
                    }
                    Increment::Custom(val) | Increment::EventCustom(val) => {
                        val.map(|incr| incr as f64)
                    }
                    Increment::FieldUnit | Increment::FieldCustom(_) => {
                        // Dropping a metric on a field will never increment.
                        // We can't increment graphql metrics unless we actually process the result.
                        // It's not like we're counting the number of requests, where we want to increment
                        // with the data that we know so far if the request stops.
                        return;
                    }
                };

                if let Some(increment) = increment {
                    histogram.record(increment, &inner.attributes);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;
    use std::path::PathBuf;
    use std::str::FromStr;

    use apollo_compiler::ast::NamedType;
    use apollo_compiler::executable::SelectionSet;
    use apollo_compiler::Name;
    use http::HeaderMap;
    use http::HeaderName;
    use http::Method;
    use http::StatusCode;
    use http::Uri;
    use multimap::MultiMap;
    use rust_embed::RustEmbed;
    use schemars::gen::SchemaGenerator;
    use serde::Deserialize;
    use serde_json::json;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Value;

    use super::*;
    use crate::context::CONTAINS_GRAPHQL_ERROR;
    use crate::context::OPERATION_KIND;
    use crate::error::Error;
    use crate::graphql;
    use crate::http_ext::TryIntoHeaderName;
    use crate::http_ext::TryIntoHeaderValue;
    use crate::json_ext::Path;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::telemetry::config_new::cache::CacheInstruments;
    use crate::plugins::telemetry::config_new::graphql::GraphQLInstruments;
    use crate::plugins::telemetry::config_new::instruments::Instrumented;
    use crate::plugins::telemetry::config_new::instruments::InstrumentsConfig;
    use crate::plugins::telemetry::APOLLO_PRIVATE_QUERY_ALIASES;
    use crate::plugins::telemetry::APOLLO_PRIVATE_QUERY_DEPTH;
    use crate::plugins::telemetry::APOLLO_PRIVATE_QUERY_HEIGHT;
    use crate::plugins::telemetry::APOLLO_PRIVATE_QUERY_ROOT_FIELDS;
    use crate::services::OperationKind;
    use crate::services::RouterRequest;
    use crate::services::RouterResponse;
    use crate::spec::operation_limits::OperationLimits;
    use crate::Context;

    type JsonMap = serde_json_bytes::Map<ByteString, Value>;

    #[derive(RustEmbed)]
    #[folder = "src/plugins/telemetry/config_new/fixtures"]
    struct Asset;

    #[derive(Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case", deny_unknown_fields)]
    enum Event {
        Extension {
            map: serde_json::Map<String, serde_json::Value>,
        },
        Context {
            map: serde_json::Map<String, serde_json::Value>,
        },
        RouterRequest {
            method: String,
            uri: String,
            #[serde(default)]
            headers: HashMap<String, String>,
            body: String,
        },
        RouterResponse {
            status: u16,
            #[serde(default)]
            headers: HashMap<String, String>,
            body: String,
        },
        RouterError {
            error: String,
        },
        SupergraphRequest {
            query: String,
            method: String,
            uri: String,
            #[serde(default)]
            headers: HashMap<String, String>,
        },
        SupergraphResponse {
            status: u16,
            #[serde(default)]
            headers: HashMap<String, String>,
            label: Option<String>,
            #[schemars(with = "Option<serde_json::Value>")]
            data: Option<Value>,
            #[schemars(with = "Option<String>")]
            path: Option<Path>,
            #[serde(default)]
            #[schemars(with = "Vec<serde_json::Value>")]
            errors: Vec<Error>,
            // Skip the `Object` type alias in order to use buildstructor’s map special-casing
            #[serde(default)]
            #[schemars(with = "Option<serde_json::Map<String, serde_json::Value>>")]
            extensions: JsonMap,
        },
        SubgraphRequest {
            subgraph_name: String,
            operation_kind: Option<OperationKind>,
            query: String,
            operation_name: Option<String>,
            #[serde(default)]
            #[schemars(with = "Option<serde_json::Map<String, serde_json::Value>>")]
            variables: JsonMap,
            #[serde(default)]
            #[schemars(with = "Option<serde_json::Map<String, serde_json::Value>>")]
            extensions: JsonMap,
            #[serde(default)]
            headers: HashMap<String, String>,
        },
        SupergraphError {
            error: String,
        },
        SubgraphResponse {
            status: u16,
            subgraph_name: Option<String>,
            data: Option<serde_json::Value>,
            #[serde(default)]
            #[schemars(with = "Option<serde_json::Map<String, serde_json::Value>>")]
            extensions: JsonMap,
            #[serde(default)]
            #[schemars(with = "Vec<serde_json::Value>")]
            errors: Vec<Error>,
            #[serde(default)]
            headers: HashMap<String, String>,
        },
        /// Note that this MUST not be used without first using supergraph request event
        GraphqlResponse {
            #[schemars(with = "Option<serde_json::Value>")]
            data: Option<Value>,
            #[schemars(with = "Option<String>")]
            path: Option<Path>,
            #[serde(default)]
            #[schemars(with = "Vec<serde_json::Value>")]
            errors: Vec<Error>,
            // Skip the `Object` type alias in order to use buildstructor’s map special-casing
            #[serde(default)]
            #[schemars(with = "Option<serde_json::Map<String, serde_json::Value>>")]
            extensions: JsonMap,
        },
        /// Note that this MUST not be used without first using supergraph request event
        ResponseField {
            typed_value: TypedValueMirror,
        },
    }

    #[derive(Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case", deny_unknown_fields)]
    enum TypedValueMirror {
        Null,
        Bool {
            type_name: String,
            field_name: String,
            field_type: String,
            value: bool,
        },
        Number {
            type_name: String,
            field_name: String,
            field_type: String,
            value: serde_json::Number,
        },
        String {
            type_name: String,
            field_name: String,
            field_type: String,
            value: String,
        },
        List {
            type_name: String,
            field_name: String,
            field_type: String,
            values: Vec<TypedValueMirror>,
        },
        Object {
            type_name: String,
            field_name: String,
            field_type: String,
            values: HashMap<String, TypedValueMirror>,
        },
        Root {
            values: HashMap<String, TypedValueMirror>,
        },
    }

    impl TypedValueMirror {
        fn field(&self) -> Option<apollo_compiler::executable::Field> {
            match self {
                TypedValueMirror::Null | TypedValueMirror::Root { .. } => None,
                TypedValueMirror::Bool {
                    field_name,
                    field_type,
                    ..
                }
                | TypedValueMirror::Number {
                    field_name,
                    field_type,
                    ..
                }
                | TypedValueMirror::String {
                    field_name,
                    field_type,
                    ..
                }
                | TypedValueMirror::List {
                    field_name,
                    field_type,
                    ..
                }
                | TypedValueMirror::Object {
                    field_name,
                    field_type,
                    ..
                } => Some(Self::create_field(field_type.clone(), field_name.clone())),
            }
        }

        fn ty(&self) -> Option<NamedType> {
            match self {
                TypedValueMirror::Null | TypedValueMirror::Root { .. } => None,
                TypedValueMirror::Bool { type_name, .. }
                | TypedValueMirror::Number { type_name, .. }
                | TypedValueMirror::String { type_name, .. }
                | TypedValueMirror::List { type_name, .. }
                | TypedValueMirror::Object { type_name, .. } => {
                    Some(Self::create_type_name(type_name.clone()))
                }
            }
        }

        fn value(&self) -> Option<Value> {
            match self {
                TypedValueMirror::Null => Some(Value::Null),
                TypedValueMirror::Bool { value, .. } => Some(serde_json_bytes::json!(*value)),
                TypedValueMirror::Number { value, .. } => Some(serde_json_bytes::json!(value)),
                TypedValueMirror::String { value, .. } => Some(serde_json_bytes::json!(value)),
                TypedValueMirror::List { values, .. } => {
                    let values = values.iter().filter_map(|v| v.value()).collect();
                    Some(Value::Array(values))
                }
                TypedValueMirror::Object { values, .. } => {
                    let values = values
                        .iter()
                        .map(|(k, v)| (k.clone().into(), v.value().unwrap_or(Value::Null)))
                        .collect();
                    Some(Value::Object(values))
                }
                TypedValueMirror::Root { values } => {
                    let values = values
                        .iter()
                        .map(|(k, v)| (k.clone().into(), v.value().unwrap_or(Value::Null)))
                        .collect();
                    Some(Value::Object(values))
                }
            }
        }

        fn create_field(
            field_type: String,
            field_name: String,
        ) -> apollo_compiler::executable::Field {
            apollo_compiler::executable::Field {
                definition: apollo_compiler::schema::FieldDefinition {
                    description: None,
                    name: NamedType::new(&field_name).expect("valid field name"),
                    arguments: vec![],
                    ty: apollo_compiler::schema::Type::Named(
                        NamedType::new(&field_type).expect("valid type name"),
                    ),
                    directives: Default::default(),
                }
                .into(),
                alias: None,
                name: NamedType::new(&field_name).expect("valid field name"),
                arguments: vec![],
                directives: Default::default(),
                selection_set: SelectionSet::new(
                    NamedType::new(&field_name).expect("valid field name"),
                ),
            }
        }

        fn create_type_name(type_name: String) -> Name {
            NamedType::new(&type_name).expect("valid type name")
        }
    }

    #[derive(Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields, rename_all = "snake_case")]
    struct TestDefinition {
        description: String,
        events: Vec<Vec<Event>>,
    }

    #[tokio::test]
    async fn test_instruments() {
        // This test is data driven.
        // It reads a list of fixtures from the fixtures directory and runs a test for each fixture.
        // Each fixture is a yaml file that contains a list of events and a router config for the instruments.

        for fixture in Asset::iter() {
            // There's no async in this test, but introducing an async block allows us to separate metrics for each fixture.
            async move {
                if fixture.ends_with("test.yaml") {
                    println!("Running test for fixture: {}", fixture);
                    let path = PathBuf::from_str(&fixture).unwrap();
                    let fixture_name = path
                        .parent()
                        .expect("fixture path")
                        .file_name()
                        .expect("fixture name");
                    let test_definition_file = Asset::get(&fixture).expect("failed to get fixture");
                    let test_definition: TestDefinition =
                        serde_yaml::from_slice(&test_definition_file.data)
                            .expect("failed to parse fixture");

                    let router_config_file =
                        Asset::get(&fixture.replace("test.yaml", "router.yaml"))
                            .expect("failed to get fixture router config");

                    let mut config = load_config(&router_config_file.data);
                    config.update_defaults();

                    for request in test_definition.events {
                        // each array of actions is a separate request
                        let mut router_instruments = None;
                        let mut supergraph_instruments = None;
                        let mut subgraph_instruments = None;
                        let mut cache_instruments: Option<CacheInstruments> = None;
                        let graphql_instruments: GraphQLInstruments = config
                            .new_graphql_instruments(Arc::new(
                                config.new_builtin_graphql_instruments(),
                            ));
                        let context = Context::new();
                        for event in request {
                            match event {
                                Event::RouterRequest {
                                    method,
                                    uri,
                                    headers,
                                    body,
                                } => {
                                    let router_req = RouterRequest::fake_builder()
                                        .context(context.clone())
                                        .method(Method::from_str(&method).expect("method"))
                                        .uri(Uri::from_str(&uri).expect("uri"))
                                        .headers(convert_headers(headers))
                                        .body(body)
                                        .build()
                                        .unwrap();
                                    router_instruments = Some(config.new_router_instruments(
                                        Arc::new(config.new_builtin_router_instruments()),
                                    ));
                                    router_instruments
                                        .as_mut()
                                        .expect("router instruments")
                                        .on_request(&router_req);
                                }
                                Event::RouterResponse {
                                    status,
                                    headers,
                                    body,
                                } => {
                                    let router_resp = RouterResponse::fake_builder()
                                        .context(context.clone())
                                        .status_code(StatusCode::from_u16(status).expect("status"))
                                        .headers(convert_headers(headers))
                                        .data(body)
                                        .build()
                                        .unwrap();
                                    router_instruments
                                        .take()
                                        .expect("router instruments")
                                        .on_response(&router_resp);
                                }
                                Event::RouterError { error } => {
                                    router_instruments
                                        .take()
                                        .expect("router request must have been made first")
                                        .on_error(&BoxError::from(error), &context);
                                }
                                Event::SupergraphRequest {
                                    query,
                                    method,
                                    uri,
                                    headers,
                                } => {
                                    supergraph_instruments =
                                        Some(config.new_supergraph_instruments(Arc::new(
                                            config.new_builtin_supergraph_instruments(),
                                        )));

                                    let mut request = supergraph::Request::fake_builder()
                                        .context(context.clone())
                                        .method(Method::from_str(&method).expect("method"))
                                        .headers(convert_headers(headers))
                                        .query(query)
                                        .build()
                                        .unwrap();
                                    *request.supergraph_request.uri_mut() =
                                        Uri::from_str(&uri).expect("uri");

                                    supergraph_instruments
                                        .as_mut()
                                        .unwrap()
                                        .on_request(&request);
                                }
                                Event::SupergraphResponse {
                                    status,
                                    label,
                                    data,
                                    path,
                                    errors,
                                    extensions,
                                    headers,
                                } => {
                                    let response = supergraph::Response::fake_builder()
                                        .context(context.clone())
                                        .status_code(StatusCode::from_u16(status).expect("status"))
                                        .and_label(label)
                                        .and_path(path)
                                        .errors(errors)
                                        .extensions(extensions)
                                        .and_data(data)
                                        .headers(convert_headers(headers))
                                        .build()
                                        .unwrap();

                                    supergraph_instruments
                                        .take()
                                        .unwrap()
                                        .on_response(&response);
                                }
                                Event::SubgraphRequest {
                                    subgraph_name,
                                    operation_kind,
                                    query,
                                    operation_name,
                                    variables,
                                    extensions,
                                    headers,
                                } => {
                                    subgraph_instruments = Some(config.new_subgraph_instruments(
                                        Arc::new(config.new_builtin_subgraph_instruments()),
                                    ));
                                    cache_instruments = Some(config.new_cache_instruments(
                                        Arc::new(config.new_builtin_cache_instruments()),
                                    ));
                                    let graphql_request = graphql::Request::fake_builder()
                                        .query(query)
                                        .and_operation_name(operation_name)
                                        .variables(variables)
                                        .extensions(extensions)
                                        .build();
                                    let mut http_request = http::Request::new(graphql_request);
                                    *http_request.headers_mut() = convert_http_headers(headers);

                                    let request = subgraph::Request::fake_builder()
                                        .context(context.clone())
                                        .subgraph_name(subgraph_name)
                                        .and_operation_kind(operation_kind)
                                        .subgraph_request(http_request)
                                        .build();

                                    subgraph_instruments.as_mut().unwrap().on_request(&request);
                                    cache_instruments.as_mut().unwrap().on_request(&request);
                                }
                                Event::SubgraphResponse {
                                    subgraph_name,
                                    status,
                                    data,
                                    extensions,
                                    errors,
                                    headers,
                                } => {
                                    let response = subgraph::Response::fake2_builder()
                                        .context(context.clone())
                                        .and_subgraph_name(subgraph_name)
                                        .status_code(StatusCode::from_u16(status).expect("status"))
                                        .and_data(data)
                                        .errors(errors)
                                        .extensions(extensions)
                                        .headers(convert_headers(headers))
                                        .build()
                                        .unwrap();
                                    subgraph_instruments
                                        .take()
                                        .expect("subgraph request must have been made first")
                                        .on_response(&response);
                                    cache_instruments
                                        .take()
                                        .expect("subgraph request must have been made first")
                                        .on_response(&response);
                                }
                                Event::SupergraphError { error } => {
                                    supergraph_instruments
                                        .take()
                                        .expect("supergraph request must have been made first")
                                        .on_error(&BoxError::from(error), &context);
                                }
                                Event::GraphqlResponse {
                                    data,
                                    path,
                                    errors,
                                    extensions,
                                } => {
                                    let response = graphql::Response::builder()
                                        .and_data(data)
                                        .and_path(path)
                                        .errors(errors)
                                        .extensions(extensions)
                                        .build();
                                    supergraph_instruments
                                        .as_mut()
                                        .expect(
                                            "supergraph request event should have happened first",
                                        )
                                        .on_response_event(&response, &context);
                                }
                                Event::ResponseField { typed_value } => {
                                    graphql_instruments.on_response_field(
                                        &typed_value.ty().expect("type should exist"),
                                        &typed_value.field().expect("field should exist"),
                                        &typed_value.value().expect("value should exist"),
                                        &context,
                                    );
                                }
                                Event::Context { map } => {
                                    for (key, value) in map {
                                        context.insert(key, value).expect("insert context");
                                    }
                                }
                                Event::Extension { map } => {
                                    for (key, value) in map {
                                        if key == APOLLO_PRIVATE_QUERY_ALIASES.to_string() {
                                            context.extensions().with_lock(|mut lock| {
                                                let limits = lock
                                                    .get_or_default_mut::<OperationLimits<u32>>();
                                                let value_as_u32 = value.as_u64().unwrap() as u32;
                                                limits.aliases = value_as_u32;
                                            });
                                        }
                                        if key == APOLLO_PRIVATE_QUERY_DEPTH.to_string() {
                                            context.extensions().with_lock(|mut lock| {
                                                let limits = lock
                                                    .get_or_default_mut::<OperationLimits<u32>>();
                                                let value_as_u32 = value.as_u64().unwrap() as u32;
                                                limits.depth = value_as_u32;
                                            });
                                        }
                                        if key == APOLLO_PRIVATE_QUERY_HEIGHT.to_string() {
                                            context.extensions().with_lock(|mut lock| {
                                                let limits = lock
                                                    .get_or_default_mut::<OperationLimits<u32>>();
                                                let value_as_u32 = value.as_u64().unwrap() as u32;
                                                limits.height = value_as_u32;
                                            });
                                        }
                                        if key == APOLLO_PRIVATE_QUERY_ROOT_FIELDS.to_string() {
                                            context.extensions().with_lock(|mut lock| {
                                                let limits = lock
                                                    .get_or_default_mut::<OperationLimits<u32>>();
                                                let value_as_u32 = value.as_u64().unwrap() as u32;
                                                limits.root_fields = value_as_u32;
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }

                    let mut snapshot_path = PathBuf::new();
                    snapshot_path.push("fixtures");
                    path.iter().for_each(|p| snapshot_path.push(p));
                    snapshot_path.pop();
                    let description = test_definition.description;
                    let info: serde_yaml::Value = serde_yaml::from_slice(&router_config_file.data)
                        .expect("failed to parse fixture");

                    insta::with_settings!({sort_maps => true,
                        snapshot_path=>snapshot_path,
                        input_file=>fixture_name,
                        prepend_module_to_snapshot=>false,
                        description=>description,
                        info=>&info
                    }, {
                        let metrics = crate::metrics::collect_metrics();
                        insta::assert_yaml_snapshot!("metrics", &metrics.all());
                    });
                }
            }
            .with_metrics()
            .await;
        }
    }

    fn convert_http_headers(headers: HashMap<String, String>) -> HeaderMap {
        let mut converted_headers = HeaderMap::new();
        for (name, value) in headers {
            converted_headers.insert::<HeaderName>(
                name.try_into().expect("expected header name"),
                value.try_into().expect("expected header value"),
            );
        }
        converted_headers
    }

    fn convert_headers(
        headers: HashMap<String, String>,
    ) -> MultiMap<TryIntoHeaderName, TryIntoHeaderValue> {
        let mut converted_headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue> =
            MultiMap::new();
        for (name, value) in headers {
            converted_headers.insert(name.into(), value.into());
        }
        converted_headers
    }

    fn load_config(config: &[u8]) -> InstrumentsConfig {
        let val: serde_json::Value = serde_yaml::from_slice(config).unwrap();
        let instruments = val
            .as_object()
            .unwrap()
            .get("telemetry")
            .unwrap()
            .as_object()
            .unwrap()
            .get("instrumentation")
            .unwrap()
            .as_object()
            .unwrap()
            .get("instruments")
            .unwrap();
        serde_json::from_value(instruments.clone()).unwrap()
    }

    #[test]
    fn write_schema() {
        // Write a json schema for the above test
        let mut schema_gen = SchemaGenerator::default();
        let schema = schema_gen.root_schema_for::<TestDefinition>();
        let schema = serde_json::to_string_pretty(&schema);
        let mut path = PathBuf::from_str(env!("CARGO_MANIFEST_DIR")).expect("manifest dir");
        path.push("src");
        path.push("plugins");
        path.push("telemetry");
        path.push("config_new");
        path.push("fixtures");
        path.push("schema.json");
        let mut file = File::create(path).unwrap();
        file.write_all(schema.unwrap().as_bytes())
            .expect("write schema");
    }

    #[tokio::test]
    async fn test_router_instruments() {
        // Please don't add further logic to this test, it's already testing multiple things.
        // Instead, add a data driven test via test_instruments test.
        async {
            let config: InstrumentsConfig = serde_json::from_str(
                json!({
                    "router": {
                        "http.server.request.body.size": true,
                        "http.server.response.body.size": {
                            "attributes": {
                                "http.response.status_code": false,
                                "acme.my_attribute": {
                                    "response_header": "x-my-header",
                                    "default": "unknown"
                                }
                            }
                        },
                        "acme.request.on_error": {
                            "value": "unit",
                            "type": "counter",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "not": {
                                    "eq": [
                                        200,
                                        {
                                            "response_status": "code"
                                        }
                                    ]
                                }
                            },
                            "attributes": {
                                "http.response.status_code": true
                            }
                        },
                        "acme.request.on_critical_error": {
                            "value": "unit",
                            "type": "counter",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "eq": [
                                    "request time out",
                                    {
                                        "error": "reason"
                                    }
                                ]
                            },
                            "attributes": {
                                "http.response.status_code": true
                            }
                        },
                        "acme.request.on_error_histo": {
                            "value": "unit",
                            "type": "histogram",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "not": {
                                    "eq": [
                                        200,
                                        {
                                            "response_status": "code"
                                        }
                                    ]
                                }
                            },
                            "attributes": {
                                "http.response.status_code": true
                            }
                        },
                        "acme.request.header_value": {
                            "value": {
                                "request_header": "x-my-header-count"
                            },
                            "type": "counter",
                            "description": "my description",
                            "unit": "nb"
                        }
                    }
                })
                .to_string()
                .as_str(),
            )
            .unwrap();

            let router_instruments =
                config.new_router_instruments(Arc::new(config.new_builtin_router_instruments()));
            let router_req = RouterRequest::fake_builder()
                .header("conditional-custom", "X")
                .header("x-my-header-count", "55")
                .header("content-length", "35")
                .header("content-type", "application/graphql")
                .build()
                .unwrap();
            router_instruments.on_request(&router_req);
            let router_response = RouterResponse::fake_builder()
                .context(router_req.context.clone())
                .status_code(StatusCode::BAD_REQUEST)
                .header("content-type", "application/json")
                .header("x-my-header", "TEST")
                .header("content-length", "35")
                .data(json!({"errors": [{"message": "nope"}]}))
                .build()
                .unwrap();
            router_instruments.on_response(&router_response);

            assert_counter!("acme.request.header_value", 55.0);
            assert_counter!(
                "acme.request.on_error",
                1.0,
                "http.response.status_code" = 400
            );
            assert_histogram_sum!(
                "acme.request.on_error_histo",
                1.0,
                "http.response.status_code" = 400
            );
            assert_histogram_sum!("http.server.request.body.size", 35.0);
            assert_histogram_sum!(
                "http.server.response.body.size",
                35.0,
                "acme.my_attribute" = "TEST"
            );

            let router_instruments =
                config.new_router_instruments(Arc::new(config.new_builtin_router_instruments()));
            let router_req = RouterRequest::fake_builder()
                .header("content-length", "35")
                .header("x-my-header-count", "5")
                .header("content-type", "application/graphql")
                .build()
                .unwrap();
            router_instruments.on_request(&router_req);
            let router_response = RouterResponse::fake_builder()
                .context(router_req.context.clone())
                .status_code(StatusCode::BAD_REQUEST)
                .header("content-type", "application/json")
                .header("content-length", "35")
                .data(json!({"errors": [{"message": "nope"}]}))
                .build()
                .unwrap();
            router_instruments.on_response(&router_response);

            assert_counter!("acme.request.header_value", 60.0);
            assert_counter!(
                "acme.request.on_error",
                2.0,
                "http.response.status_code" = 400
            );
            assert_histogram_sum!(
                "acme.request.on_error_histo",
                2.0,
                "http.response.status_code" = 400
            );
            assert_histogram_sum!("http.server.request.body.size", 70.0);
            assert_histogram_sum!(
                "http.server.response.body.size",
                35.0,
                "acme.my_attribute" = "TEST"
            );
            assert_histogram_sum!(
                "http.server.response.body.size",
                35.0,
                "acme.my_attribute" = "unknown"
            );

            let router_instruments =
                config.new_router_instruments(Arc::new(config.new_builtin_router_instruments()));
            let router_req = RouterRequest::fake_builder()
                .header("content-length", "35")
                .header("content-type", "application/graphql")
                .build()
                .unwrap();
            router_instruments.on_request(&router_req);
            let router_response = RouterResponse::fake_builder()
                .context(router_req.context.clone())
                .status_code(StatusCode::OK)
                .header("content-type", "application/json")
                .header("content-length", "35")
                .data(json!({"errors": [{"message": "nope"}]}))
                .build()
                .unwrap();
            router_instruments.on_response(&router_response);

            assert_counter!("acme.request.header_value", 60.0);
            assert_counter!(
                "acme.request.on_error",
                2.0,
                "http.response.status_code" = 400
            );
            assert_histogram_sum!(
                "acme.request.on_error_histo",
                2.0,
                "http.response.status_code" = 400
            );

            let router_instruments =
                config.new_router_instruments(Arc::new(config.new_builtin_router_instruments()));
            let router_req = RouterRequest::fake_builder()
                .header("content-length", "35")
                .header("content-type", "application/graphql")
                .build()
                .unwrap();
            router_instruments.on_request(&router_req);
            router_instruments.on_error(&BoxError::from("request time out"), &Context::new());
            assert_counter!(
                "acme.request.on_critical_error",
                1.0,
                "http.response.status_code" = 500
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_supergraph_instruments() {
        // Please don't add further logic to this test, it's already testing multiple things.
        // Instead, add a data driven test via test_instruments test.
        async {
            let config: InstrumentsConfig = serde_json::from_str(
                json!({
                    "supergraph": {
                        "acme.request.on_error": {
                            "value": "unit",
                            "type": "counter",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "not": {
                                    "eq": [
                                        200,
                                        {
                                            "response_status": "code"
                                        }
                                    ]
                                }
                            }
                        },
                        "acme.request.on_graphql_error": {
                            "value": "event_unit",
                            "type": "counter",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "eq": [
                                    "NOPE",
                                    {
                                        "response_errors": "$.[0].extensions.code"
                                    }
                                ]
                            },
                            "attributes": {
                                "response_errors": {
                                    "response_errors": "$.*"
                                }
                            }
                        },
                        "acme.request.on_graphql_error_selector": {
                            "value": "event_unit",
                            "type": "counter",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "eq": [
                                    true,
                                    {
                                        "on_graphql_error": true
                                    }
                                ]
                            },
                            "attributes": {
                                "response_errors": {
                                    "response_errors": "$.*"
                                }
                            }
                        },
                        "acme.request.on_graphql_error_histo": {
                            "value": "event_unit",
                            "type": "histogram",
                            "unit": "error",
                            "description": "my description",
                            "condition": {
                                "eq": [
                                    "NOPE",
                                    {
                                        "response_errors": "$.[0].extensions.code"
                                    }
                                ]
                            },
                            "attributes": {
                                "response_errors": {
                                    "response_errors": "$.*"
                                }
                            }
                        },
                        "acme.request.on_graphql_data": {
                            "value": {
                                "response_data": "$.price"
                            },
                            "type": "counter",
                            "unit": "$",
                            "description": "my description",
                            "attributes": {
                                "response.data": {
                                    "response_data": "$.*"
                                }
                            }
                        },
                        "acme.query": {
                            "value": "unit",
                            "type": "counter",
                            "description": "nb of queries",
                            "condition": {
                                "eq": [
                                    "query",
                                    {
                                        "operation_kind": "string"
                                    }
                                ]
                            },
                            "unit": "query",
                            "attributes": {
                                "query": {
                                    "query": "string"
                                }
                            }
                        }
                    }
                })
                .to_string()
                .as_str(),
            )
            .unwrap();

            let custom_instruments = SupergraphCustomInstruments::new(
                &config.supergraph.custom,
                Arc::new(config.new_builtin_supergraph_instruments()),
            );
            let context = crate::context::Context::new();
            let _ = context.insert(OPERATION_KIND, "query".to_string()).unwrap();
            let context_with_error = crate::context::Context::new();
            let _ = context_with_error
                .insert(OPERATION_KIND, "query".to_string())
                .unwrap();
            let _ = context_with_error
                .insert(CONTAINS_GRAPHQL_ERROR, true)
                .unwrap();
            let supergraph_req = supergraph::Request::fake_builder()
                .header("conditional-custom", "X")
                .header("x-my-header-count", "55")
                .header("content-length", "35")
                .header("content-type", "application/graphql")
                .query("{me{name}}")
                .context(context.clone())
                .build()
                .unwrap();
            custom_instruments.on_request(&supergraph_req);
            let supergraph_response = supergraph::Response::fake_builder()
                .context(supergraph_req.context.clone())
                .status_code(StatusCode::BAD_REQUEST)
                .header("content-type", "application/json")
                .header("x-my-header", "TEST")
                .header("content-length", "35")
                .errors(vec![graphql::Error::builder()
                    .message("nope")
                    .extension_code("NOPE")
                    .build()])
                .build()
                .unwrap();
            custom_instruments.on_response(&supergraph_response);
            custom_instruments.on_response_event(
                &graphql::Response::builder()
                    .data(json!({
                        "price": 500
                    }))
                    .errors(vec![graphql::Error::builder()
                        .message("nope")
                        .extension_code("NOPE")
                        .build()])
                    .build(),
                &context_with_error,
            );

            assert_counter!("acme.query", 1.0, query = "{me{name}}");
            assert_counter!("acme.request.on_error", 1.0);
            assert_counter!(
                "acme.request.on_graphql_error",
                1.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!(
                "acme.request.on_graphql_error_selector",
                1.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_histogram_sum!(
                "acme.request.on_graphql_error_histo",
                1.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!("acme.request.on_graphql_data", 500.0, response.data = 500);

            let custom_instruments = SupergraphCustomInstruments::new(
                &config.supergraph.custom,
                Arc::new(config.new_builtin_supergraph_instruments()),
            );
            let supergraph_req = supergraph::Request::fake_builder()
                .header("content-length", "35")
                .header("x-my-header-count", "5")
                .header("content-type", "application/graphql")
                .context(context.clone())
                .query("Subscription {me{name}}")
                .build()
                .unwrap();
            custom_instruments.on_request(&supergraph_req);
            let supergraph_response = supergraph::Response::fake_builder()
                .context(supergraph_req.context.clone())
                .status_code(StatusCode::BAD_REQUEST)
                .header("content-type", "application/json")
                .header("content-length", "35")
                .errors(vec![graphql::Error::builder()
                    .message("nope")
                    .extension_code("NOPE")
                    .build()])
                .build()
                .unwrap();
            custom_instruments.on_response(&supergraph_response);
            custom_instruments.on_response_event(
                &graphql::Response::builder()
                    .data(json!({
                        "price": 500
                    }))
                    .errors(vec![graphql::Error::builder()
                        .message("nope")
                        .extension_code("NOPE")
                        .build()])
                    .build(),
                &context_with_error,
            );

            assert_counter!("acme.query", 1.0, query = "{me{name}}");
            assert_counter!("acme.request.on_error", 2.0);
            assert_counter!(
                "acme.request.on_graphql_error",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!(
                "acme.request.on_graphql_error_selector",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_histogram_sum!(
                "acme.request.on_graphql_error_histo",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!("acme.request.on_graphql_data", 1000.0, response.data = 500);

            let custom_instruments = SupergraphCustomInstruments::new(
                &config.supergraph.custom,
                Arc::new(config.new_builtin_supergraph_instruments()),
            );
            let supergraph_req = supergraph::Request::fake_builder()
                .header("content-length", "35")
                .header("content-type", "application/graphql")
                .context(context.clone())
                .query("{me{name}}")
                .build()
                .unwrap();
            custom_instruments.on_request(&supergraph_req);
            let supergraph_response = supergraph::Response::fake_builder()
                .context(supergraph_req.context.clone())
                .status_code(StatusCode::OK)
                .header("content-type", "application/json")
                .header("content-length", "35")
                .data(serde_json_bytes::json!({"foo": "bar"}))
                .build()
                .unwrap();
            custom_instruments.on_response(&supergraph_response);
            custom_instruments.on_response_event(
                &graphql::Response::builder()
                    .data(serde_json_bytes::json!({"foo": "bar"}))
                    .build(),
                &supergraph_req.context,
            );

            assert_counter!("acme.query", 2.0, query = "{me{name}}");
            assert_counter!("acme.request.on_error", 2.0);
            assert_counter!(
                "acme.request.on_graphql_error",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!(
                "acme.request.on_graphql_error_selector",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_histogram_sum!(
                "acme.request.on_graphql_error_histo",
                2.0,
                response_errors = "{\"message\":\"nope\",\"extensions\":{\"code\":\"NOPE\"}}"
            );
            assert_counter!("acme.request.on_graphql_data", 1000.0, response.data = 500);
        }
        .with_metrics()
        .await;
    }
}
