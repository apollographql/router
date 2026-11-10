use std::collections::HashMap;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::sync::Arc;

use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;
use opentelemetry::metrics::Histogram;
use opentelemetry::metrics::MeterProvider;
use opentelemetry::metrics::ObservableGauge;
use opentelemetry::metrics::UpDownCounter;
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

use super::DefaultForLevel;
use super::Selector;
use super::cache::CacheInstrumentSet;
use super::cache::CacheInstrumentsConfig;
use super::cache::attributes::CacheAttributes;
use super::graphql::FIELD_EXECUTION;
use super::graphql::FIELD_LENGTH;
use super::graphql::GraphQLInstrumentSet;
use super::graphql::selectors::ListLength;
use super::http_server::attributes::HttpServerAttributes;
use super::router::instruments::RouterInstrumentSet;
use super::router::instruments::RouterInstrumentsConfig;
use super::router_overhead;
use super::selectors::CacheKind;
use super::subgraph::instruments::SubgraphInstrumentSet;
use super::subgraph::instruments::SubgraphInstrumentsConfig;
use super::supergraph::instruments::SupergraphCustomInstruments;
use super::supergraph::instruments::SupergraphInstrumentsConfig;
use crate::Context;
use crate::metrics;
use crate::plugins::subscription::SUBSCRIPTION_SUBGRAPH_NAME_CONTEXT_KEY;
use crate::plugins::telemetry::CLIENT_NAME;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::apollo::instruments::ApolloConnectorInstrumentSet;
use crate::plugins::telemetry::config_new::apollo::instruments::ApolloConnectorInstruments;
use crate::plugins::telemetry::config_new::apollo::instruments::ApolloSubgraphInstrumentSet;
use crate::plugins::telemetry::config_new::apollo::instruments::ApolloSubgraphInstruments;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::cache::RESPONSE_CACHE_METRIC;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::connector::attributes::ConnectorAttributes;
use crate::plugins::telemetry::config_new::connector::instruments::ConnectorInstrumentSet;
use crate::plugins::telemetry::config_new::connector::instruments::ConnectorInstruments;
use crate::plugins::telemetry::config_new::connector::instruments::ConnectorInstrumentsConfig;
use crate::plugins::telemetry::config_new::connector::selectors::ConnectorSelector;
use crate::plugins::telemetry::config_new::connector::selectors::ConnectorValue;
use crate::plugins::telemetry::config_new::cost::CostInstrumentSet;
use crate::plugins::telemetry::config_new::cost::CostInstruments;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::graphql::GraphQLInstrumentsConfig;
use crate::plugins::telemetry::config_new::graphql::attributes::GraphQLAttributes;
use crate::plugins::telemetry::config_new::graphql::selectors::GraphQLSelector;
use crate::plugins::telemetry::config_new::graphql::selectors::GraphQLValue;
use crate::plugins::telemetry::config_new::router::attributes::RouterAttributes;
use crate::plugins::telemetry::config_new::router::selectors::RouterSelector;
use crate::plugins::telemetry::config_new::router::selectors::RouterValue;
use crate::plugins::telemetry::config_new::subgraph::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphSelector;
use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphValue;
use crate::plugins::telemetry::config_new::supergraph::attributes::SupergraphAttributes;
use crate::plugins::telemetry::config_new::supergraph::selectors::SupergraphSelector;
use crate::plugins::telemetry::config_new::supergraph::selectors::SupergraphValue;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::plugins::telemetry::utils::extend_attributes;
use crate::services::router;
use crate::services::supergraph;

pub(crate) const METER_NAME: &str = "apollo/router";

#[apollo_configuration::configuration]
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
    /// Connector service instruments.
    pub(crate) connector: Extendable<
        ConnectorInstrumentsConfig,
        Instrument<ConnectorAttributes, ConnectorSelector, ConnectorValue>,
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

pub(crate) const HTTP_SERVER_REQUEST_DURATION_METRIC: &str = "http.server.request.duration";
const HTTP_SERVER_REQUEST_BODY_SIZE_METRIC: &str = "http.server.request.body.size";
const HTTP_SERVER_RESPONSE_BODY_SIZE_METRIC: &str = "http.server.response.body.size";
const HTTP_SERVER_ACTIVE_REQUESTS: &str = "http.server.active_requests";

pub(super) const HTTP_CLIENT_REQUEST_DURATION_METRIC: &str = "http.client.request.duration";
pub(super) const HTTP_CLIENT_REQUEST_BODY_SIZE_METRIC: &str = "http.client.request.body.size";
pub(super) const HTTP_CLIENT_RESPONSE_BODY_SIZE_METRIC: &str = "http.client.response.body.size";

pub(super) const APOLLO_ROUTER_OPERATIONS_FETCH_DURATION: &str =
    "apollo.router.operations.fetch.duration";

pub(super) const APOLLO_ROUTER_OPERATIONS_SUBSCRIPTIONS_TERMINATED: &str =
    "apollo.router.operations.subscriptions.terminated.client";

impl InstrumentsConfig {
    pub(crate) fn validate_selectors(&self) -> Result<(), String> {
        for (name, custom) in &self.router.custom {
            custom.condition.validate_selectors(None).map_err(|err| {
                format!("error for custom router instrument {name:?} in condition: {err}")
            })?;
        }
        for (name, custom) in &self.supergraph.custom {
            custom.condition.validate_selectors(None).map_err(|err| {
                format!("error for custom supergraph instrument {name:?} in condition: {err}")
            })?;
        }
        for (name, custom) in &self.subgraph.custom {
            custom.condition.validate_selectors(None).map_err(|err| {
                format!("error for custom subgraph instrument {name:?} in condition: {err}")
            })?;
        }
        for (name, custom) in &self.graphql.custom {
            custom.condition.validate_selectors(None).map_err(|err| {
                format!("error for custom graphql instrument {name:?} in condition: {err}")
            })?;
        }
        for (name, custom) in &self.cache.custom {
            custom.condition.validate_selectors(None).map_err(|err| {
                format!("error for custom cache instrument {name:?} in condition: {err}")
            })?;
        }
        for (name, custom) in &self.connector.custom {
            custom.condition.validate_selectors(None).map_err(|err| {
                format!("error for custom connector instrument {name:?} in condition: {err}")
            })?;
        }

        Ok(())
    }

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
        self.connector
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
                        .with_unit("s")
                        .with_description("Duration of HTTP server requests.")
                        .build(),
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
                        .with_unit("By")
                        .with_description("Size of HTTP server request bodies.")
                        .build(),
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
                        .with_unit("By")
                        .with_description("Size of HTTP server response bodies.")
                        .build(),
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
                        .with_unit("request")
                        .with_description("Number of active HTTP server requests.")
                        .build(),
                ),
            );
        }

        if let Some((name, instrument)) = router_overhead::instruments::create_static_instrument(
            self.router.attributes.router_overhead.is_enabled(),
        ) {
            static_instruments.insert(name, instrument);
        }

        if self.router.attributes.subscriptions_terminated.is_enabled() {
            static_instruments.insert(
                APOLLO_ROUTER_OPERATIONS_SUBSCRIPTIONS_TERMINATED.to_string(),
                StaticInstrument::CounterF64(
                    meter
                        .f64_counter(APOLLO_ROUTER_OPERATIONS_SUBSCRIPTIONS_TERMINATED)
                        .with_description("Count of subscription terminations")
                        .build(),
                ),
            );
        }

        for (instrument_name, instrument) in &self.router.custom {
            match instrument.ty {
                InstrumentType::Counter => {
                    static_instruments.insert(
                        instrument_name.to_string(),
                        StaticInstrument::CounterF64(
                            meter
                                .f64_counter(instrument_name.to_string())
                                .with_description(instrument.description.clone())
                                .with_unit(instrument.unit.clone())
                                .build(),
                        ),
                    );
                }
                InstrumentType::Histogram => {
                    static_instruments.insert(
                        instrument_name.to_string(),
                        StaticInstrument::Histogram(
                            meter
                                .f64_histogram(instrument_name.to_string())
                                .with_description(instrument.description.clone())
                                .with_unit(instrument.unit.clone())
                                .build(),
                        ),
                    );
                }
            }
        }

        static_instruments
    }

    /// The router instruments this configuration configures, built once for a pipeline over its
    /// `static_instruments`; [`RouterInstrumentSet::start`] makes those one request records into.
    pub(crate) fn router_instrument_set(
        &self,
        static_instruments: &HashMap<String, StaticInstrument>,
    ) -> RouterInstrumentSet {
        let attributes = &self.router.attributes;
        let static_instrument = |name: &str| {
            static_instruments
                .get(name)
                .expect("cannot get static instrument for router; this should not happen")
        };
        let histogram = |name: &str| {
            static_instrument(name)
                .as_histogram()
                .cloned()
                .expect("cannot convert instrument to histogram for router; this should not happen")
        };
        let standard = |instrument: &DefaultedStandardInstrument<
            Extendable<RouterAttributes, RouterSelector>,
        >,
                        name: &str,
                        increment: IncrementKind,
                        selector: Option<RouterSelector>| {
            instrument.is_enabled().then(|| {
                HistogramInstrument::new(
                    increment,
                    histogram(name),
                    selector.map(Arc::new),
                    match instrument {
                        DefaultedStandardInstrument::Bool(_)
                        | DefaultedStandardInstrument::Unset => None,
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            Some(attributes.clone())
                        }
                    },
                )
            })
        };
        RouterInstrumentSet {
            http_server_request_duration: standard(
                &attributes.http_server_request_duration,
                HTTP_SERVER_REQUEST_DURATION_METRIC,
                IncrementKind::Duration(DurationUnit::Seconds),
                None,
            ),
            http_server_request_body_size: standard(
                &attributes.http_server_request_body_size,
                HTTP_SERVER_REQUEST_BODY_SIZE_METRIC,
                IncrementKind::Custom,
                Some(RouterSelector::RequestHeader {
                    request_header: "content-length".to_string(),
                    redact: None,
                    default: None,
                }),
            ),
            // Compressed sizes are not yet known at the time on_response is called, so this will
            // always be the uncompressed size.
            http_server_response_body_size: standard(
                &attributes.http_server_response_body_size,
                HTTP_SERVER_RESPONSE_BODY_SIZE_METRIC,
                IncrementKind::Custom,
                Some(RouterSelector::ResponseSizeHint {
                    response_size_hint: true,
                }),
            ),
            http_server_active_requests: attributes.http_server_active_requests.is_enabled().then(
                || ActiveRequestsInstrument {
                    counter: static_instrument(HTTP_SERVER_ACTIVE_REQUESTS)
                        .as_up_down_counter_i64()
                        .cloned()
                        .expect(
                            "cannot convert instrument to up and down counter for router; this should not happen",
                        ),
                    attrs_config: match &attributes.http_server_active_requests {
                        DefaultedStandardInstrument::Bool(_)
                        | DefaultedStandardInstrument::Unset => Default::default(),
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            attributes.clone()
                        }
                    },
                },
            ),
            router_overhead: router_overhead::instruments::router_overhead_instrument(
                &attributes.router_overhead,
                static_instruments,
            ),
            subscriptions_terminated: attributes.subscriptions_terminated.is_enabled().then(|| {
                SubscriptionsTerminatedInstrument {
                    counter: static_instrument(APOLLO_ROUTER_OPERATIONS_SUBSCRIPTIONS_TERMINATED)
                        .as_counter_f64()
                        .cloned()
                        .expect(
                            "cannot convert instrument to counter for subscriptions terminated; this should not happen",
                        ),
                    selectors: match &attributes.subscriptions_terminated {
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            attributes.clone()
                        }
                        _ => Arc::new(Extendable {
                            attributes: SubscriptionsTerminatedAttributes::default(),
                            custom: HashMap::new(),
                        }),
                    },
                }
            }),
            custom: CustomInstrumentSet::new(&self.router.custom, static_instruments),
        }
    }

    pub(crate) fn new_builtin_supergraph_instruments(&self) -> HashMap<String, StaticInstrument> {
        let meter = metrics::meter_provider().meter(METER_NAME);

        let mut static_instruments = HashMap::with_capacity(self.supergraph.custom.len());
        for (instrument_name, instrument) in &self.supergraph.custom {
            match instrument.ty {
                InstrumentType::Counter => {
                    static_instruments.insert(
                        instrument_name.to_string(),
                        StaticInstrument::CounterF64(
                            meter
                                .f64_counter(instrument_name.to_string())
                                .with_description(instrument.description.clone())
                                .with_unit(instrument.unit.clone())
                                .build(),
                        ),
                    );
                }
                InstrumentType::Histogram => {
                    static_instruments.insert(
                        instrument_name.to_string(),
                        StaticInstrument::Histogram(
                            meter
                                .f64_histogram(instrument_name.to_string())
                                .with_description(instrument.description.clone())
                                .with_unit(instrument.unit.clone())
                                .build(),
                        ),
                    );
                }
            }
        }
        static_instruments.extend(self.supergraph.attributes.cost.new_static_instruments());

        static_instruments
    }

    /// The supergraph instruments this configuration configures, built once for a pipeline over
    /// its `static_instruments`; [`SupergraphInstrumentSet::start`] makes those one request
    /// records into.
    pub(crate) fn supergraph_instrument_set(
        &self,
        static_instruments: &HashMap<String, StaticInstrument>,
    ) -> SupergraphInstrumentSet {
        SupergraphInstrumentSet {
            cost: self
                .supergraph
                .attributes
                .cost
                .instrument_set(static_instruments),
            custom: CustomInstrumentSet::new(&self.supergraph.custom, static_instruments),
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
                        .with_unit("s")
                        .with_description("Duration of HTTP client requests.")
                        .build(),
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
                        .with_unit("By")
                        .with_description("Size of HTTP client request bodies.")
                        .build(),
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
                        .with_unit("By")
                        .with_description("Size of HTTP client response bodies.")
                        .build(),
                ),
            );
        }

        for (instrument_name, instrument) in &self.subgraph.custom {
            match instrument.ty {
                InstrumentType::Counter => {
                    static_instruments.insert(
                        instrument_name.to_string(),
                        StaticInstrument::CounterF64(
                            meter
                                .f64_counter(instrument_name.to_string())
                                .with_description(instrument.description.clone())
                                .with_unit(instrument.unit.clone())
                                .build(),
                        ),
                    );
                }
                InstrumentType::Histogram => {
                    static_instruments.insert(
                        instrument_name.to_string(),
                        StaticInstrument::Histogram(
                            meter
                                .f64_histogram(instrument_name.to_string())
                                .with_description(instrument.description.clone())
                                .with_unit(instrument.unit.clone())
                                .build(),
                        ),
                    );
                }
            }
        }

        static_instruments
    }

    /// The subgraph instruments this configuration configures, built once for a pipeline over
    /// its `static_instruments`; [`SubgraphInstrumentSet::start`] makes those one subgraph request
    /// records into.
    pub(crate) fn subgraph_instrument_set(
        &self,
        static_instruments: &HashMap<String, StaticInstrument>,
    ) -> SubgraphInstrumentSet {
        let attributes = &self.subgraph.attributes;
        let standard = |instrument: &DefaultedStandardInstrument<
            Extendable<SubgraphAttributes, SubgraphSelector>,
        >,
                        name: &str,
                        increment: IncrementKind,
                        selector: Option<SubgraphSelector>| {
            instrument.is_enabled().then(|| {
                HistogramInstrument::new(
                    increment,
                    static_instruments
                        .get(name)
                        .expect("cannot get static instrument for subgraph; this should not happen")
                        .as_histogram()
                        .cloned()
                        .expect(
                            "cannot convert instrument to histogram for subgraph; this should not happen",
                        ),
                    selector.map(Arc::new),
                    match instrument {
                        DefaultedStandardInstrument::Bool(_) | DefaultedStandardInstrument::Unset => {
                            None
                        }
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            Some(attributes.clone())
                        }
                    },
                )
            })
        };
        SubgraphInstrumentSet {
            http_client_request_duration: standard(
                &attributes.http_client_request_duration,
                HTTP_CLIENT_REQUEST_DURATION_METRIC,
                IncrementKind::Duration(DurationUnit::Seconds),
                None,
            ),
            http_client_request_body_size: standard(
                &attributes.http_client_request_body_size,
                HTTP_CLIENT_REQUEST_BODY_SIZE_METRIC,
                IncrementKind::Custom,
                Some(SubgraphSelector::SubgraphRequestBodySize {
                    subgraph_request_body_size: true,
                }),
            ),
            http_client_response_body_size: standard(
                &attributes.http_client_response_body_size,
                HTTP_CLIENT_RESPONSE_BODY_SIZE_METRIC,
                IncrementKind::Custom,
                Some(SubgraphSelector::SubgraphResponseBodySize {
                    subgraph_response_body_size: true,
                }),
            ),
            custom: CustomInstrumentSet::new(&self.subgraph.custom, static_instruments),
        }
    }

    pub(crate) fn new_builtin_apollo_subgraph_instruments(
        &self,
    ) -> HashMap<String, StaticInstrument> {
        ApolloSubgraphInstruments::new_builtin()
    }

    /// Apollo's subgraph instruments, built once for a pipeline over its `static_instruments`,
    /// where `subgraph_metrics`; [`ApolloSubgraphInstrumentSet::start`] makes those one subgraph request
    /// records into.
    pub(crate) fn apollo_subgraph_instrument_set(
        &self,
        static_instruments: &HashMap<String, StaticInstrument>,
        subgraph_metrics: bool,
    ) -> ApolloSubgraphInstrumentSet {
        ApolloSubgraphInstrumentSet::new(static_instruments, subgraph_metrics)
    }

    /// The connector instruments this configuration configures, built once for a pipeline over
    /// its `static_instruments`; [`ConnectorInstrumentSet::start`] makes those one connector request
    /// records into.
    pub(crate) fn connector_instrument_set(
        &self,
        static_instruments: &HashMap<String, StaticInstrument>,
    ) -> ConnectorInstrumentSet {
        ConnectorInstrumentSet::new(&self.connector, static_instruments)
    }

    pub(crate) fn new_builtin_connector_instruments(&self) -> HashMap<String, StaticInstrument> {
        let meter = metrics::meter_provider().meter(METER_NAME);
        let mut static_instruments = ConnectorInstruments::new_builtin(&self.connector);

        for (instrument_name, instrument) in &self.connector.custom {
            match instrument.ty {
                InstrumentType::Counter => {
                    static_instruments.insert(
                        instrument_name.to_string(),
                        StaticInstrument::CounterF64(
                            meter
                                .f64_counter(instrument_name.to_string())
                                .with_description(instrument.description.clone())
                                .with_unit(instrument.unit.clone())
                                .build(),
                        ),
                    );
                }
                InstrumentType::Histogram => {
                    static_instruments.insert(
                        instrument_name.to_string(),
                        StaticInstrument::Histogram(
                            meter
                                .f64_histogram(instrument_name.to_string())
                                .with_description(instrument.description.clone())
                                .with_unit(instrument.unit.clone())
                                .build(),
                        ),
                    );
                }
            }
        }

        static_instruments
    }

    pub(crate) fn new_builtin_apollo_connector_instruments(
        &self,
    ) -> HashMap<String, StaticInstrument> {
        ApolloConnectorInstruments::new_builtin()
    }

    /// Apollo's connector instruments, built once for a pipeline over its `static_instruments`,
    /// where `subgraph_metrics`; [`ApolloConnectorInstrumentSet::start`] makes those one connector request
    /// records into.
    pub(crate) fn apollo_connector_instrument_set(
        &self,
        static_instruments: &HashMap<String, StaticInstrument>,
        subgraph_metrics: bool,
    ) -> ApolloConnectorInstrumentSet {
        ApolloConnectorInstrumentSet::new(static_instruments, subgraph_metrics)
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
                        .build(),
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
                        .build(),
                ),
            );
        }

        for (instrument_name, instrument) in &self.graphql.custom {
            match instrument.ty {
                InstrumentType::Counter => {
                    static_instruments.insert(
                        instrument_name.to_string(),
                        StaticInstrument::CounterF64(
                            meter
                                .f64_counter(instrument_name.to_string())
                                .with_description(instrument.description.clone())
                                .with_unit(instrument.unit.clone())
                                .build(),
                        ),
                    );
                }
                InstrumentType::Histogram => {
                    static_instruments.insert(
                        instrument_name.to_string(),
                        StaticInstrument::Histogram(
                            meter
                                .f64_histogram(instrument_name.to_string())
                                .with_description(instrument.description.clone())
                                .with_unit(instrument.unit.clone())
                                .build(),
                        ),
                    );
                }
            }
        }

        static_instruments
    }

    /// The GraphQL instruments this configuration configures, built once for a pipeline over its
    /// `static_instruments`; [`GraphQLInstrumentSet::start`] makes those one request records
    /// into.
    pub(crate) fn graphql_instrument_set(
        &self,
        static_instruments: &HashMap<String, StaticInstrument>,
    ) -> GraphQLInstrumentSet {
        let attributes = &self.graphql.attributes;
        let selectors = |instrument: &DefaultedStandardInstrument<
            Extendable<GraphQLAttributes, GraphQLSelector>,
        >| match instrument {
            DefaultedStandardInstrument::Bool(_) | DefaultedStandardInstrument::Unset => None,
            DefaultedStandardInstrument::Extendable { attributes } => Some(attributes.clone()),
        };
        let static_instrument = |name: &str| {
            static_instruments
                .get(name)
                .expect("cannot get static instrument for graphql; this should not happen")
        };
        GraphQLInstrumentSet {
            list_length: attributes.list_length.is_enabled().then(|| {
                HistogramInstrument::new(
                    IncrementKind::FieldCustom,
                    static_instrument(FIELD_LENGTH).as_histogram().cloned().expect(
                        "cannot convert instrument to histogram for graphql; this should not happen",
                    ),
                    Some(Arc::new(GraphQLSelector::ListLength {
                        list_length: ListLength::Value,
                    })),
                    selectors(&attributes.list_length),
                )
            }),
            field_execution: attributes.field_execution.is_enabled().then(|| {
                CounterInstrument::new(
                    IncrementKind::FieldUnit,
                    static_instrument(FIELD_EXECUTION)
                        .as_counter_f64()
                        .cloned()
                        .expect(
                            "cannot convert instrument to counter for graphql; this should not happen",
                        ),
                    None,
                    selectors(&attributes.field_execution),
                )
            }),
            custom: CustomInstrumentSet::new(&self.graphql.custom, static_instruments),
        }
    }

    pub(crate) fn new_builtin_cache_instruments(&self) -> HashMap<String, StaticInstrument> {
        let meter = metrics::meter_provider().meter(METER_NAME);
        let mut static_instruments: HashMap<String, StaticInstrument> = HashMap::new();
        if self.cache.attributes.response_cache.is_enabled() {
            static_instruments.insert(
                RESPONSE_CACHE_METRIC.to_string(),
                StaticInstrument::CounterF64(
                    meter
                        .f64_counter(RESPONSE_CACHE_METRIC)
                        .with_unit("ops")
                        .with_description(
                            "Response cache hit/miss operations at the subgraph level",
                        )
                        .build(),
                ),
            );
        }

        static_instruments
    }

    /// The cache instruments this configuration configures, built once for a pipeline over its
    /// `static_instruments`; [`CacheInstrumentSet::start`] makes those one subgraph request
    /// records into.
    pub(crate) fn cache_instrument_set(
        &self,
        static_instruments: &HashMap<String, StaticInstrument>,
    ) -> CacheInstrumentSet {
        let response_cache = &self.cache.attributes.response_cache;
        CacheInstrumentSet {
            cache_hit_response_cache: response_cache.is_enabled().then(|| {
                CounterInstrument::new(
                    IncrementKind::Custom,
                    static_instruments
                        .get(RESPONSE_CACHE_METRIC)
                        .expect("cannot get static instrument for cache; this should not happen")
                        .as_counter_f64()
                        .cloned()
                        .expect(
                            "cannot convert instrument to counter for cache; this should not happen",
                        ),
                    Some(Arc::new(SubgraphSelector::ResponseCache {
                        response_cache: CacheKind::Hit,
                        entity_type: None,
                    })),
                    match response_cache {
                        DefaultedStandardInstrument::Bool(_) | DefaultedStandardInstrument::Unset => {
                            None
                        }
                        DefaultedStandardInstrument::Extendable { attributes } => {
                            Some(attributes.clone())
                        }
                    },
                )
            }),
        }
    }

    /// The connector response cache counter this configuration configures, resolved once for a
    /// pipeline over its `static_instruments`; each connector request records into it through
    /// its own [`ConnectorCacheInstruments`].
    pub(crate) fn connector_cache_counter(
        &self,
        static_instruments: &HashMap<String, StaticInstrument>,
    ) -> Option<Counter<f64>> {
        if self.cache.attributes.response_cache.is_enabled() {
            static_instruments
                .get(RESPONSE_CACHE_METRIC)
                .and_then(|s| s.as_counter_f64().cloned())
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub(crate) enum StaticInstrument {
    CounterF64(Counter<f64>),
    UpDownCounterI64(UpDownCounter<i64>),
    Histogram(Histogram<f64>),
    // Gauges are never read
    #[allow(dead_code)]
    GaugeU64(ObservableGauge<u64>),
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

#[apollo_configuration::configuration]
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

#[apollo_configuration::configuration]
pub(crate) struct SubscriptionsTerminatedAttributes {
    /// The reason the subscription terminated
    reason: Option<bool>,
    /// The subgraph name
    #[serde(rename = "subgraph.name")]
    subgraph_name: Option<bool>,
    /// The client name
    #[serde(rename = "client.name")]
    client_name: Option<bool>,
}

impl SubscriptionsTerminatedAttributes {
    fn reason(&self) -> bool {
        self.reason.unwrap_or(false)
    }
}

impl SubscriptionsTerminatedAttributes {
    fn collect_attrs(&self, ctx: &Context) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        if self.subgraph_name.unwrap_or(false) {
            let name = ctx
                .get::<_, String>(SUBSCRIPTION_SUBGRAPH_NAME_CONTEXT_KEY)
                .ok()
                .flatten()
                .unwrap_or_default();
            attrs.push(KeyValue::new("subgraph.name", name));
        }
        if self.client_name.unwrap_or(false) {
            let client_name = ctx
                .get::<_, String>(CLIENT_NAME)
                .ok()
                .flatten()
                .unwrap_or_default();
            attrs.push(KeyValue::new("client.name", client_name));
        }
        attrs
    }
}

impl Selectors<router::Request, router::Response, ()> for SubscriptionsTerminatedAttributes {
    fn on_request(&self, _request: &router::Request) -> Vec<KeyValue> {
        Vec::new()
    }

    fn on_response(&self, response: &router::Response) -> Vec<KeyValue> {
        self.collect_attrs(&response.context)
    }

    fn on_error(&self, _error: &BoxError, ctx: &Context) -> Vec<KeyValue> {
        self.collect_attrs(ctx)
    }
}

impl DefaultForLevel for SubscriptionsTerminatedAttributes {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        _kind: TelemetryDataKind,
    ) {
        match requirement_level {
            DefaultAttributeRequirementLevel::Required => {
                self.reason.get_or_insert(true);
                self.subgraph_name.get_or_insert(true);
            }
            DefaultAttributeRequirementLevel::Recommended
            | DefaultAttributeRequirementLevel::None => {}
        }
    }
}

/// The subscriptions terminated counter of a router instrument set, built once for its
/// pipeline: the counter and its selectors. [`Self::start`] makes the counter one request
/// stashes and records into.
pub(crate) struct SubscriptionsTerminatedInstrument {
    pub(crate) counter: Counter<f64>,
    pub(crate) selectors: Arc<Extendable<SubscriptionsTerminatedAttributes, RouterSelector>>,
}

impl SubscriptionsTerminatedInstrument {
    pub(crate) fn start(&self) -> SubscriptionsTerminatedCounter {
        SubscriptionsTerminatedCounter::new(self.counter.clone(), self.selectors.clone())
    }
}

/// Handle stashed in the request context so that
/// [`Multipart`](crate::protocols::multipart::Multipart) can record the
/// `apollo.router.operations.subscriptions.terminated.client` counter at drop time.
///
/// Selector-based attributes are collected during `on_response` and stored in
/// `stashed_attributes` (shared via `Arc` across clones). The termination `reason`
/// is added when the subscription stream is dropped.
#[derive(Clone, Debug)]
pub(crate) struct SubscriptionsTerminatedCounter {
    counter: Counter<f64>,
    selectors: Arc<Extendable<SubscriptionsTerminatedAttributes, RouterSelector>>,
    stashed_attributes: Arc<Mutex<Vec<KeyValue>>>,
}

impl SubscriptionsTerminatedCounter {
    pub(crate) fn new(
        counter: Counter<f64>,
        selectors: Arc<Extendable<SubscriptionsTerminatedAttributes, RouterSelector>>,
    ) -> Self {
        Self {
            counter,
            selectors,
            stashed_attributes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn record(&self, reason: &str) {
        let mut attrs = self.stashed_attributes.lock().clone();
        if self.selectors.attributes.reason() {
            attrs.push(KeyValue::new("reason", reason.to_string()));
        }
        self.counter.add(1.0, &attrs);
    }

    #[cfg(test)]
    pub(crate) fn with_stashed_attributes(self, attributes: Vec<KeyValue>) -> Self {
        *self.stashed_attributes.lock() = attributes;
        self
    }
}

impl Instrumented for SubscriptionsTerminatedCounter {
    type Request = router::Request;
    type Response = router::Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        *self.stashed_attributes.lock() = self.selectors.on_request(request);
        request
            .context
            .extensions()
            .with_lock(|ext| ext.insert(self.clone()));
    }

    fn on_response(&self, response: &Self::Response) {
        let new_attrs = self.selectors.on_response(response);
        extend_attributes(&mut self.stashed_attributes.lock(), new_attrs);
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        let new_attrs = self.selectors.on_error(error, ctx);
        extend_attributes(&mut self.stashed_attributes.lock(), new_attrs);
    }
}

#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, untagged)]
#[schemars(rename = "StandardInstrument{T}")]
pub(crate) enum DefaultedStandardInstrument<T> {
    #[default]
    Unset,
    Bool(bool),
    Extendable {
        attributes: Arc<T>,
    },
}

// The configuration attribute doesn't support generic types, so this forwards validation to every
// field. The enum is untagged, so its fields sit directly under the instrument's key.
impl<T> apollo_configuration::Validate for DefaultedStandardInstrument<T>
where
    T: apollo_configuration::Validate,
{
    fn validate(&self, mut errors: apollo_configuration::ErrorCollector<'_>) {
        match self {
            Self::Unset => {}
            Self::Bool(enabled) => enabled.validate(errors),
            Self::Extendable { attributes } => attributes.validate(errors.nest("attributes")),
        }
    }
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

impl<T, Request, Response, EventResponse> Selectors<Request, Response, EventResponse>
    for DefaultedStandardInstrument<T>
where
    T: Selectors<Request, Response, EventResponse>,
{
    fn on_request(&self, request: &Request) -> Vec<opentelemetry::KeyValue> {
        match self {
            Self::Bool(_) | Self::Unset => Vec::with_capacity(0),
            Self::Extendable { attributes } => attributes.on_request(request),
        }
    }

    fn on_response(&self, response: &Response) -> Vec<opentelemetry::KeyValue> {
        match self {
            Self::Bool(_) | Self::Unset => Vec::with_capacity(0),
            Self::Extendable { attributes } => attributes.on_response(response),
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) -> Vec<opentelemetry::KeyValue> {
        match self {
            Self::Bool(_) | Self::Unset => Vec::with_capacity(0),
            Self::Extendable { attributes } => attributes.on_error(error, ctx),
        }
    }

    fn on_response_event(&self, response: &EventResponse, ctx: &Context) -> Vec<KeyValue> {
        match self {
            Self::Bool(_) | Self::Unset => Vec::with_capacity(0),
            Self::Extendable { attributes } => attributes.on_response_event(response, ctx),
        }
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

impl<A, E, Request, Response, EventResponse, SelectorValue>
    Selectors<Request, Response, EventResponse> for Instrument<A, E, SelectorValue>
where
    A: Debug + Default + Selectors<Request, Response, EventResponse>,
    E: Debug + Selector<Request = Request, Response = Response, EventResponse = EventResponse>,
    for<'a> &'a SelectorValue: Into<InstrumentValue<E>>,
{
    fn on_request(&self, request: &Request) -> Vec<opentelemetry::KeyValue> {
        self.attributes.on_request(request)
    }

    fn on_response(&self, response: &Response) -> Vec<opentelemetry::KeyValue> {
        self.attributes.on_response(response)
    }

    fn on_response_event(
        &self,
        response: &EventResponse,
        ctx: &Context,
    ) -> Vec<opentelemetry::KeyValue> {
        self.attributes.on_response_event(response, ctx)
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) -> Vec<opentelemetry::KeyValue> {
        self.attributes.on_error(error, ctx)
    }
}

// The configuration attribute doesn't support generic types, so this forwards validation to every
// field. Destructuring makes a new field a compile error until it is validated here.
impl<A, E, V> apollo_configuration::Validate for Instrument<A, E, V>
where
    A: Default + Debug + apollo_configuration::Validate,
    E: Debug + apollo_configuration::Validate,
    V: apollo_configuration::Validate,
    for<'a> &'a V: Into<InstrumentValue<E>>,
{
    fn validate(&self, mut errors: apollo_configuration::ErrorCollector<'_>) {
        let Self {
            ty,
            value,
            description,
            unit,
            attributes,
            condition,
        } = self;
        ty.validate(errors.nest("type"));
        value.validate(errors.nest("value"));
        description.validate(errors.nest("description"));
        unit.validate(errors.nest("unit"));
        attributes.validate(errors.nest("attributes"));
        condition.validate(errors.nest("condition"));
    }
}

#[apollo_configuration::configuration]
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

#[apollo_configuration::configuration]
pub(crate) enum StandardUnit {
    Unit,
}

#[apollo_configuration::configuration]
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

// The configuration attribute doesn't support generic types, so this forwards validation to each
// variant's value. The enum is untagged, so each value keeps its own path.
impl<T> apollo_configuration::Validate for InstrumentValue<T>
where
    T: apollo_configuration::Validate,
{
    fn validate(&self, errors: apollo_configuration::ErrorCollector<'_>) {
        match self {
            InstrumentValue::Standard(standard) => standard.validate(errors),
            InstrumentValue::Chunked(event) => event.validate(errors),
            InstrumentValue::Field(field) => field.validate(errors),
            InstrumentValue::Custom(selector) => selector.validate(errors),
        }
    }
}

// The configuration attribute doesn't support generic types, so this forwards validation to the
// custom selector.
impl<T> apollo_configuration::Validate for Event<T>
where
    T: apollo_configuration::Validate,
{
    fn validate(&self, mut errors: apollo_configuration::ErrorCollector<'_>) {
        match self {
            Event::Duration | Event::Unit => {}
            Event::Custom(selector) => selector.validate(errors.nest("event_custom")),
        }
    }
}

// The configuration attribute doesn't support generic types, so this forwards validation to the
// custom selector.
impl<T> apollo_configuration::Validate for Field<T>
where
    T: apollo_configuration::Validate,
{
    fn validate(&self, mut errors: apollo_configuration::ErrorCollector<'_>) {
        match self {
            Field::Unit => {}
            Field::Custom(selector) => selector.validate(errors.nest("field_custom")),
        }
    }
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

/// Instruments a layer started, or none where nothing reads them (`layers::Readers`), which
/// evaluate and record nothing.
impl<T: Instrumented> Instrumented for Option<T> {
    type Request = T::Request;
    type Response = T::Response;
    type EventResponse = T::EventResponse;

    fn on_request(&self, request: &Self::Request) {
        if let Some(instruments) = self {
            instruments.on_request(request);
        }
    }

    fn on_response(&self, response: &Self::Response) {
        if let Some(instruments) = self {
            instruments.on_response(response);
        }
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        if let Some(instruments) = self {
            instruments.on_response_event(response, ctx);
        }
    }

    fn on_response_field(
        &self,
        ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &Value,
        ctx: &Context,
    ) {
        if let Some(instruments) = self {
            instruments.on_response_field(ty, field, value, ctx);
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        if let Some(instruments) = self {
            instruments.on_error(error, ctx);
        }
    }
}

impl<A, B, E, Request, Response, EventResponse, SelectorValue> Instrumented
    for Extendable<A, Instrument<B, E, SelectorValue>>
where
    A: Default
        + Instrumented<Request = Request, Response = Response, EventResponse = EventResponse>,
    B: Default + Debug + Selectors<Request, Response, EventResponse>,
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

pub(crate) struct CustomInstruments<
    Request,
    Response,
    EventResponse,
    Attributes,
    Select,
    SelectorValue,
> where
    Attributes: Selectors<Request, Response, EventResponse> + Default,
    Select: Selector<Request = Request, Response = Response> + Debug,
{
    _phantom: PhantomData<SelectorValue>,
    counters: Vec<CustomCounter<Request, Response, EventResponse, Attributes, Select>>,
    histograms: Vec<CustomHistogram<Request, Response, EventResponse, Attributes, Select>>,
}

impl<Request, Response, EventResponse, Attributes, Select, SelectorValue>
    CustomInstruments<Request, Response, EventResponse, Attributes, Select, SelectorValue>
where
    Attributes: Selectors<Request, Response, EventResponse> + Default,
    Select: Selector<Request = Request, Response = Response> + Debug,
{
    pub(crate) fn is_empty(&self) -> bool {
        self.counters.is_empty() && self.histograms.is_empty()
    }
}

/// The custom instruments a configuration configures, built once for a pipeline:
/// [`Self::start`] makes the instruments one request or subgraph request records into.
pub(crate) struct CustomInstrumentSet<Attributes, Select, SelectorValue>
where
    Attributes: Default,
{
    counters: Vec<CounterInstrument<Attributes, Select>>,
    histograms: Vec<HistogramInstrument<Attributes, Select>>,
    _phantom: PhantomData<fn() -> SelectorValue>,
}

impl<Attributes, Select, SelectorValue> CustomInstrumentSet<Attributes, Select, SelectorValue>
where
    Attributes: Selectors<Select::Request, Select::Response, Select::EventResponse>
        + Default
        + Debug
        + Clone,
    Select: Selector + Debug + Clone,
    for<'a> &'a SelectorValue: Into<InstrumentValue<Select>>,
{
    pub(crate) fn new(
        config: &HashMap<Arc<str>, Instrument<Attributes, Select, SelectorValue>>,
        static_instruments: &HashMap<String, StaticInstrument>,
    ) -> Self {
        let mut counters = Vec::new();
        let mut histograms = Vec::new();

        for (instrument_name, instrument) in config {
            let (selector, increment) = match (&instrument.value).into() {
                InstrumentValue::Standard(Standard::Duration) => (
                    None,
                    IncrementKind::Duration(DurationUnit::named(&instrument.unit)),
                ),
                InstrumentValue::Standard(Standard::Unit) => (None, IncrementKind::Unit),
                InstrumentValue::Custom(selector) => {
                    (Some(Arc::new(selector)), IncrementKind::Custom)
                }
                InstrumentValue::Chunked(Event::Duration) => (
                    None,
                    IncrementKind::EventDuration(DurationUnit::named(&instrument.unit)),
                ),
                InstrumentValue::Chunked(Event::Unit) => (None, IncrementKind::EventUnit),
                InstrumentValue::Chunked(Event::Custom(selector)) => {
                    (Some(Arc::new(selector)), IncrementKind::EventCustom)
                }
                InstrumentValue::Field(Field::Unit) => (None, IncrementKind::FieldUnit),
                InstrumentValue::Field(Field::Custom(selector)) => {
                    (Some(Arc::new(selector)), IncrementKind::FieldCustom)
                }
            };
            let static_instrument = static_instruments
                .get(&**instrument_name)
                .expect("cannot get static instrument for supergraph; this should not happen");
            match instrument.ty {
                InstrumentType::Counter => counters.push(CounterInstrument {
                    increment,
                    condition: instrument.condition.clone(),
                    counter: static_instrument
                        .as_counter_f64()
                        .cloned()
                        .expect("cannot convert static instrument into a counter, this is a bug"),
                    selector,
                    selectors: Some(instrument.attributes.clone()),
                }),
                InstrumentType::Histogram => histograms.push(HistogramInstrument {
                    increment,
                    condition: instrument.condition.clone(),
                    histogram: static_instrument
                        .as_histogram()
                        .cloned()
                        .expect("cannot convert static instrument into a histogram, this is a bug"),
                    selector,
                    selectors: Some(instrument.attributes.clone()),
                }),
            }
        }

        Self {
            counters,
            histograms,
            _phantom: PhantomData,
        }
    }

    /// The instruments one request or subgraph request records into.
    pub(crate) fn start(
        &self,
    ) -> CustomInstruments<
        Select::Request,
        Select::Response,
        Select::EventResponse,
        Attributes,
        Select,
        SelectorValue,
    > {
        CustomInstruments {
            _phantom: PhantomData,
            counters: self.counters.iter().map(CounterInstrument::start).collect(),
            histograms: self
                .histograms
                .iter()
                .map(HistogramInstrument::start)
                .collect(),
        }
    }
}

/// A counter of an instrument set, built once for its pipeline: the instrument, how its value
/// grows, its condition and its selectors. [`Self::start`] makes the counter one request or
/// subgraph request records into.
pub(crate) struct CounterInstrument<A, T>
where
    A: Default,
{
    increment: IncrementKind,
    condition: Condition<T>,
    counter: Counter<f64>,
    selector: Option<Arc<T>>,
    selectors: Option<Arc<Extendable<A, T>>>,
}

impl<A, T> CounterInstrument<A, T>
where
    A: Selectors<T::Request, T::Response, T::EventResponse> + Default,
    T: Selector + Debug + Clone,
{
    /// A counter whose value grows by `increment`, counted with `selectors`' attributes, its
    /// value `selector`'s where it has one, and always counted.
    pub(crate) fn new(
        increment: IncrementKind,
        counter: Counter<f64>,
        selector: Option<Arc<T>>,
        selectors: Option<Arc<Extendable<A, T>>>,
    ) -> Self {
        Self {
            increment,
            condition: Condition::True,
            counter,
            selector,
            selectors,
        }
    }

    pub(crate) fn start(&self) -> CustomCounter<T::Request, T::Response, T::EventResponse, A, T> {
        CustomCounter {
            inner: Mutex::new(CustomCounterInner {
                increment: self.increment.start(),
                condition: self.condition.clone(),
                counter: Some(self.counter.clone()),
                attributes: Vec::new(),
                selector: self.selector.clone(),
                selectors: self.selectors.clone(),
                incremented: false,
                _phantom: PhantomData,
            }),
        }
    }
}

/// A histogram of an instrument set, built once for its pipeline: the instrument, how its value
/// grows, its condition and its selectors. [`Self::start`] makes the histogram one request or
/// subgraph request records into.
pub(crate) struct HistogramInstrument<A, T>
where
    A: Default,
{
    increment: IncrementKind,
    condition: Condition<T>,
    histogram: Histogram<f64>,
    selector: Option<Arc<T>>,
    selectors: Option<Arc<Extendable<A, T>>>,
}

impl<A, T> HistogramInstrument<A, T>
where
    A: Selectors<T::Request, T::Response, T::EventResponse> + Default,
    T: Selector + Clone,
{
    /// A histogram whose value grows by `increment`, recorded with `selectors`' attributes, its
    /// value `selector`'s where it has one, and always recorded.
    pub(crate) fn new(
        increment: IncrementKind,
        histogram: Histogram<f64>,
        selector: Option<Arc<T>>,
        selectors: Option<Arc<Extendable<A, T>>>,
    ) -> Self {
        Self {
            increment,
            condition: Condition::True,
            histogram,
            selector,
            selectors,
        }
    }

    pub(crate) fn start(&self) -> CustomHistogram<T::Request, T::Response, T::EventResponse, A, T> {
        CustomHistogram {
            inner: Mutex::new(CustomHistogramInner {
                increment: self.increment.start(),
                condition: self.condition.clone(),
                histogram: Some(self.histogram.clone()),
                attributes: Vec::new(),
                selector: self.selector.clone(),
                selectors: self.selectors.clone(),
                updated: false,
                _phantom: PhantomData,
            }),
        }
    }
}

impl<Request, Response, EventResponse, Attributes, Select, SelectorValue> Instrumented
    for CustomInstruments<Request, Response, EventResponse, Attributes, Select, SelectorValue>
where
    Attributes: Selectors<Request, Response, EventResponse> + Default,
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

/// The supergraph instruments a configuration configures, built once for a pipeline
/// (`InstrumentsConfig::supergraph_instrument_set`).
pub(crate) struct SupergraphInstrumentSet {
    cost: CostInstrumentSet,
    custom: CustomInstrumentSet<SupergraphAttributes, SupergraphSelector, SupergraphValue>,
}

impl SupergraphInstrumentSet {
    /// The instruments one request records into.
    pub(crate) fn start(&self) -> SupergraphInstruments {
        SupergraphInstruments {
            cost: self.cost.start(),
            custom: self.custom.start(),
        }
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

// ---------------- Counter -----------------------
#[derive(Debug, Clone)]
pub(crate) enum Increment {
    Unit,
    EventUnit,
    FieldUnit,
    Duration(Instant, DurationUnit),
    EventDuration(Instant, DurationUnit),
    Custom(Option<opentelemetry::Value>),
    EventCustom(Option<opentelemetry::Value>),
    FieldCustom(Option<opentelemetry::Value>),
}

fn value_to_f64(value: &opentelemetry::Value) -> Option<f64> {
    match value {
        opentelemetry::Value::F64(f) => Some(*f),
        opentelemetry::Value::I64(i) => Some(*i as f64),
        opentelemetry::Value::String(s) => s.as_str().parse::<f64>().ok(),
        opentelemetry::Value::Bool(_) => None,
        opentelemetry::Value::Array(_) => None,
        _ => unreachable!("unexpected opentelemetry::Value variant"),
    }
}

/// The unit an instrument records a duration in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DurationUnit {
    Seconds,
    Milliseconds,
    Microseconds,
    Nanoseconds,
}

impl DurationUnit {
    /// The unit an instrument's configured `unit` names: "ms", "us" and "ns", and seconds for
    /// "s" and any other.
    pub(crate) fn named(unit: &str) -> Self {
        match unit {
            "ms" => Self::Milliseconds,
            "us" => Self::Microseconds,
            "ns" => Self::Nanoseconds,
            _ => Self::Seconds,
        }
    }
}

/// `duration` in `unit`.
fn duration_to_f64(duration: std::time::Duration, unit: &DurationUnit) -> f64 {
    match unit {
        DurationUnit::Milliseconds => duration.as_secs_f64() * 1000.0,
        DurationUnit::Microseconds => duration.as_micros() as f64,
        DurationUnit::Nanoseconds => duration.as_nanos() as f64,
        DurationUnit::Seconds => duration.as_secs_f64(),
    }
}

fn duration_to_value(duration: std::time::Duration, unit: &DurationUnit) -> opentelemetry::Value {
    opentelemetry::Value::F64(duration_to_f64(duration, unit))
}

/// How an instrument's value grows, as its configuration fixes it for its pipeline;
/// [`Self::start`] is the increment a request or a subgraph request records from.
#[derive(Clone, Copy, Debug)]
pub(crate) enum IncrementKind {
    Unit,
    EventUnit,
    FieldUnit,
    Duration(DurationUnit),
    EventDuration(DurationUnit),
    Custom,
    EventCustom,
    FieldCustom,
}

impl IncrementKind {
    /// The increment one request or subgraph request starts from, a duration measured from now.
    pub(crate) fn start(self) -> Increment {
        match self {
            Self::Unit => Increment::Unit,
            Self::EventUnit => Increment::EventUnit,
            Self::FieldUnit => Increment::FieldUnit,
            Self::Duration(unit) => Increment::Duration(Instant::now(), unit),
            Self::EventDuration(unit) => Increment::EventDuration(Instant::now(), unit),
            Self::Custom => Increment::Custom(None),
            Self::EventCustom => Increment::EventCustom(None),
            Self::FieldCustom => Increment::FieldCustom(None),
        }
    }
}

pub(crate) struct CustomCounter<Request, Response, EventResponse, A, T>
where
    A: Selectors<Request, Response, EventResponse> + Default,
    T: Selector<Request = Request, Response = Response> + Debug,
{
    pub(crate) inner: Mutex<CustomCounterInner<Request, Response, EventResponse, A, T>>,
}

impl<Request, Response, EventResponse, A, T> Clone
    for CustomCounter<Request, Response, EventResponse, A, T>
where
    A: Selectors<Request, Response, EventResponse> + Default,
    T: Selector<Request = Request, Response = Response> + Debug + Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: Mutex::new(self.inner.lock().clone()),
        }
    }
}

pub(crate) struct CustomCounterInner<Request, Response, EventResponse, A, T>
where
    A: Selectors<Request, Response, EventResponse> + Default,
    T: Selector<Request = Request, Response = Response> + Debug,
{
    pub(crate) increment: Increment,
    pub(crate) selector: Option<Arc<T>>,
    pub(crate) selectors: Option<Arc<Extendable<A, T>>>,
    pub(crate) counter: Option<Counter<f64>>,
    pub(crate) condition: Condition<T>,
    pub(crate) attributes: Vec<opentelemetry::KeyValue>,
    // Useful when it's a counter on events to know if we have to count for an event or not
    pub(crate) incremented: bool,
    pub(crate) _phantom: PhantomData<EventResponse>,
}

impl<Request, Response, EventResponse, A, T> Clone
    for CustomCounterInner<Request, Response, EventResponse, A, T>
where
    A: Selectors<Request, Response, EventResponse> + Default,
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
            _phantom: PhantomData,
        }
    }
}

impl<A, T, Request, Response, EventResponse> Instrumented
    for CustomCounter<Request, Response, EventResponse, A, T>
where
    A: Selectors<Request, Response, EventResponse> + Default,
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
                Increment::EventCustom(None) => Increment::EventCustom(Some(selected_value)),
                Increment::Custom(None) => Increment::Custom(Some(selected_value)),
                other => {
                    tracing::error!(
                        "this is a bug and should not happen, the increment should only be Custom or EventCustom: {other:?}"
                    );
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
                    | Increment::EventDuration(_, _)
                    | Increment::EventUnit
                    | Increment::FieldCustom(_)
                    | Increment::FieldUnit
            ) {
                let _ = inner.counter.take();
            }
            return;
        }

        let attrs = inner
            .selectors
            .as_ref()
            .map(|s| s.on_response(response))
            .unwrap_or_default();
        extend_attributes(&mut inner.attributes, attrs);

        if let Some(selected_value) = inner
            .selector
            .as_ref()
            .and_then(|s| s.on_response(response))
        {
            let new_incr = match &inner.increment {
                Increment::EventCustom(None) => Increment::Custom(Some(selected_value)),
                Increment::Custom(None) => Increment::Custom(Some(selected_value)),
                other => {
                    tracing::error!(
                        "this is a bug and should not happen, the increment should only be Custom or EventCustom: {other:?}"
                    );
                    return;
                }
            };
            inner.increment = new_incr;
        }

        if let Some(increment) = match &inner.increment {
            Increment::Unit => Some(opentelemetry::Value::F64(1.0)),
            Increment::Duration(instant, unit) => Some(duration_to_value(instant.elapsed(), unit)),
            Increment::Custom(val) => val.clone(),
            Increment::EventUnit
            | Increment::EventDuration(_, _)
            | Increment::EventCustom(_)
            | Increment::FieldUnit
            | Increment::FieldCustom(_) => {
                // Nothing to do because we're incrementing on events or fields
                return;
            }
        } {
            if let Some(counter) = &inner.counter
                && let Some(value) = value_to_f64(&increment)
            {
                counter.add(value, &inner.attributes);
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
            extend_attributes(&mut attrs, selectors.on_response_event(response, ctx));
        }

        if let Some(selected_value) = inner
            .selector
            .as_ref()
            .and_then(|s| s.on_response_event(response, ctx))
        {
            let new_incr = match &inner.increment {
                Increment::EventCustom(None) => Increment::EventCustom(Some(selected_value)),
                Increment::Custom(None) => Increment::EventCustom(Some(selected_value)),
                other => {
                    tracing::error!(
                        "this is a bug and should not happen, the increment should only be Custom or EventCustom: {other:?}"
                    );
                    return;
                }
            };
            inner.increment = new_incr;
        }

        let increment = match &mut inner.increment {
            Increment::EventUnit => 1f64,
            Increment::EventDuration(instant, unit) => {
                let incr = duration_to_f64(instant.elapsed(), unit);
                // Set it to new instant for the next event
                *instant = Instant::now();
                incr
            }
            Increment::Custom(val) | Increment::EventCustom(val) => {
                let incr = match val {
                    Some(incr) => value_to_f64(incr).unwrap_or(0f64),
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
            extend_attributes(&mut attrs, selectors.on_error(error, ctx));
        }

        let increment = match &inner.increment {
            Increment::Unit | Increment::EventUnit | Increment::FieldUnit => {
                opentelemetry::Value::F64(1.0)
            }
            Increment::Duration(instant, unit) | Increment::EventDuration(instant, unit) => {
                duration_to_value(instant.elapsed(), unit)
            }
            Increment::Custom(val) | Increment::EventCustom(val) | Increment::FieldCustom(val) => {
                val.as_ref()
                    .cloned()
                    .unwrap_or(opentelemetry::Value::F64(0.0))
            }
        };

        if let Some(counter) = inner.counter.take()
            && let Some(value) = value_to_f64(&increment)
        {
            counter.add(value, &attrs);
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
                Increment::FieldCustom(None) => Increment::FieldCustom(Some(selected_value)),
                Increment::Custom(None) => Increment::FieldCustom(Some(selected_value)),
                other => {
                    tracing::error!(
                        "this is a bug and should not happen, the increment should only be Custom or FieldCustom: {other:?}"
                    );
                    return;
                }
            };
            inner.increment = new_incr;
        }

        let increment: Option<opentelemetry::Value> = match &mut inner.increment {
            Increment::FieldUnit => Some(opentelemetry::Value::F64(1.0)),
            Increment::FieldCustom(val) => val.take(),
            Increment::Unit
            | Increment::Duration(_, _)
            | Increment::Custom(_)
            | Increment::EventDuration(_, _)
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

        if let (Some(counter), Some(increment)) = (&inner.counter, increment.as_ref()) {
            if let Some(value) = value_to_f64(increment) {
                counter.add(value, &inner.attributes);
            }
            // Reset the attributes to the original length, this will discard the new attributes added from selectors.
            inner.attributes.truncate(original_length);
        }
    }
}

impl<A, T, Request, Response, EventResponse> Drop
    for CustomCounter<Request, Response, EventResponse, A, T>
where
    A: Selectors<Request, Response, EventResponse> + Default,
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
            if let Some(counter) = inner.counter.take()
                && let Some(incr) = match &inner.increment {
                    Increment::Unit | Increment::EventUnit => Some(1f64),
                    Increment::Duration(instant, unit)
                    | Increment::EventDuration(instant, unit) => {
                        Some(duration_to_f64(instant.elapsed(), unit))
                    }
                    Increment::Custom(val) | Increment::EventCustom(val) => {
                        val.as_ref().and_then(value_to_f64)
                    }
                    Increment::FieldUnit | Increment::FieldCustom(_) => {
                        // Dropping a metric on a field will never increment.
                        // We can't increment graphql metrics unless we actually process the result.
                        // It's not like we're counting the number of requests, where we want to increment
                        // with the data that we know so far if the request stops.
                        return;
                    }
                }
            {
                counter.add(incr, &inner.attributes);
            }
        }
    }
}

/// The active requests counter of a router instrument set, built once for its pipeline: the
/// counter and the attributes it records. [`Self::start`] makes the counter one request records
/// into.
pub(crate) struct ActiveRequestsInstrument {
    pub(crate) counter: UpDownCounter<i64>,
    pub(crate) attrs_config: Arc<ActiveRequestsAttributes>,
}

impl ActiveRequestsInstrument {
    pub(crate) fn start(&self) -> ActiveRequestsCounter {
        ActiveRequestsCounter {
            inner: Mutex::new(ActiveRequestsCounterInner {
                counter: Some(self.counter.clone()),
                attrs_config: self.attrs_config.clone(),
                attributes: Vec::new(),
            }),
        }
    }
}

pub(crate) struct ActiveRequestsCounter {
    inner: Mutex<ActiveRequestsCounterInner>,
}

struct ActiveRequestsCounterInner {
    counter: Option<UpDownCounter<i64>>,
    attrs_config: Arc<ActiveRequestsAttributes>,
    attributes: Vec<opentelemetry::KeyValue>,
}

impl Instrumented for ActiveRequestsCounter {
    type Request = router::Request;
    type Response = router::Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        let mut inner = self.inner.lock();
        if inner.attrs_config.http_request_method
            && let Some(attr) = (RouterSelector::RequestMethod {
                request_method: true,
            })
            .on_request(request)
        {
            inner
                .attributes
                .push(KeyValue::new(HTTP_REQUEST_METHOD, attr));
        }
        if inner.attrs_config.server_address
            && let Some(attr) = HttpServerAttributes::forwarded_host(request)
                .and_then(|h| h.host().map(|h| h.to_string()))
        {
            inner.attributes.push(KeyValue::new(SERVER_ADDRESS, attr));
        }
        if inner.attrs_config.server_port
            && let Some(attr) =
                HttpServerAttributes::forwarded_host(request).and_then(|h| h.port_u16())
        {
            inner
                .attributes
                .push(KeyValue::new(SERVER_PORT, attr as i64));
        }
        if inner.attrs_config.url_scheme
            && let Some(attr) = request.router_request.uri().scheme_str()
        {
            inner
                .attributes
                .push(KeyValue::new(URL_SCHEME, attr.to_string()));
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
        if let Some(mut inner) = inner
            && let Some(counter) = &inner.counter.take()
        {
            counter.add(-1, &inner.attributes);
        }
    }
}

// ---------------- Histogram -----------------------

pub(crate) struct CustomHistogram<Request, Response, EventResponse, A, T>
where
    A: Selectors<Request, Response, EventResponse> + Default,
    T: Selector<Request = Request, Response = Response>,
{
    pub(crate) inner: Mutex<CustomHistogramInner<Request, Response, EventResponse, A, T>>,
}

pub(crate) struct CustomHistogramInner<Request, Response, EventResponse, A, T>
where
    A: Selectors<Request, Response, EventResponse> + Default,
    T: Selector<Request = Request, Response = Response>,
{
    pub(crate) increment: Increment,
    pub(crate) condition: Condition<T>,
    pub(crate) selector: Option<Arc<T>>,
    pub(crate) selectors: Option<Arc<Extendable<A, T>>>,
    pub(crate) histogram: Option<Histogram<f64>>,
    pub(crate) attributes: Vec<opentelemetry::KeyValue>,
    // Useful when it's an histogram on events to know if we have to count for an event or not
    pub(crate) updated: bool,
    pub(crate) _phantom: PhantomData<EventResponse>,
}

impl<A, T, Request, Response, EventResponse> Instrumented
    for CustomHistogram<Request, Response, EventResponse, A, T>
where
    A: Selectors<Request, Response, EventResponse> + Default,
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
                Increment::EventCustom(None) => Increment::EventCustom(Some(selected_value)),
                Increment::FieldCustom(None) => Increment::FieldCustom(Some(selected_value)),
                Increment::Custom(None) => Increment::Custom(Some(selected_value)),
                other => {
                    tracing::error!(
                        "this is a bug and should not happen, the increment should only be Custom or EventCustom: {other:?}"
                    );
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
                    | Increment::EventDuration(_, _)
                    | Increment::EventUnit
                    | Increment::FieldCustom(_)
                    | Increment::FieldUnit
            ) {
                let _ = inner.histogram.take();
            }
            return;
        }
        let attrs = inner
            .selectors
            .as_ref()
            .map(|s| s.on_response(response))
            .unwrap_or_default();
        extend_attributes(&mut inner.attributes, attrs);
        if let Some(selected_value) = inner
            .selector
            .as_ref()
            .and_then(|s| s.on_response(response))
        {
            let new_incr = match &inner.increment {
                Increment::EventCustom(None) => Increment::EventCustom(Some(selected_value)),
                Increment::FieldCustom(None) => Increment::FieldCustom(Some(selected_value)),
                Increment::Custom(None) => Increment::Custom(Some(selected_value)),
                other => {
                    tracing::error!(
                        "this is a bug and should not happen, the increment should only be Custom or EventCustom: {other:?}"
                    );
                    return;
                }
            };
            inner.increment = new_incr;
        }

        let increment = match &inner.increment {
            Increment::Unit => Some(opentelemetry::Value::F64(1.0)),
            Increment::Duration(instant, unit) => Some(duration_to_value(instant.elapsed(), unit)),
            Increment::Custom(val) => val.clone(),
            Increment::EventUnit
            | Increment::EventDuration(_, _)
            | Increment::EventCustom(_)
            | Increment::FieldUnit
            | Increment::FieldCustom(_) => {
                // Nothing to do because we're incrementing on events or fields
                return;
            }
        };

        if let (Some(histogram), Some(increment)) = (&inner.histogram, increment.as_ref()) {
            if let Some(value) = value_to_f64(increment) {
                histogram.record(value, &inner.attributes);
            }
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
            extend_attributes(&mut attrs, selectors.on_response_event(response, ctx));
        }

        if let Some(selected_value) = inner
            .selector
            .as_ref()
            .and_then(|s| s.on_response_event(response, ctx))
        {
            let new_incr = match &inner.increment {
                Increment::EventCustom(None) => Increment::EventCustom(Some(selected_value)),
                Increment::Custom(None) => Increment::EventCustom(Some(selected_value)),
                other => {
                    tracing::error!(
                        "this is a bug and should not happen, the increment should only be Custom or EventCustom: {other:?}"
                    );
                    return;
                }
            };
            inner.increment = new_incr;
        }

        let increment: Option<opentelemetry::Value> = match &mut inner.increment {
            Increment::EventUnit => Some(opentelemetry::Value::F64(1.0)),
            Increment::EventDuration(instant, unit) => {
                let incr = Some(duration_to_value(instant.elapsed(), unit));
                // Need a new instant for the next event
                *instant = Instant::now();
                incr
            }
            Increment::EventCustom(val) => val.take(),
            Increment::Unit
            | Increment::Duration(_, _)
            | Increment::Custom(_)
            | Increment::FieldUnit
            | Increment::FieldCustom(_) => {
                // Nothing to do because we're incrementing on events
                return;
            }
        };
        if let (Some(histogram), Some(increment)) = (&inner.histogram, increment.as_ref()) {
            if let Some(value) = value_to_f64(increment) {
                histogram.record(value, &attrs);
            }
            inner.updated = true;
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        let mut inner = self.inner.lock();
        let mut attrs = inner.attributes.clone();
        if let Some(selectors) = inner.selectors.as_ref() {
            extend_attributes(&mut attrs, selectors.on_error(error, ctx));
        }

        let increment = match &inner.increment {
            Increment::Unit | Increment::EventUnit | Increment::FieldUnit => {
                Some(opentelemetry::Value::F64(1.0))
            }
            Increment::Duration(instant, unit) | Increment::EventDuration(instant, unit) => {
                Some(duration_to_value(instant.elapsed(), unit))
            }
            Increment::Custom(val) | Increment::EventCustom(val) | Increment::FieldCustom(val) => {
                val.clone()
            }
        };

        if let (Some(histogram), Some(increment)) = (inner.histogram.take(), increment.as_ref())
            && let Some(value) = value_to_f64(increment)
        {
            histogram.record(value, &attrs);
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
                Increment::FieldCustom(None) => Increment::FieldCustom(Some(selected_value)),
                Increment::Custom(None) => Increment::FieldCustom(Some(selected_value)),
                other => {
                    tracing::error!(
                        "this is a bug and should not happen, the increment should only be Custom or FieldCustom: {other:?}"
                    );
                    return;
                }
            };
            inner.increment = new_incr;
        }

        let increment: Option<opentelemetry::Value> = match &mut inner.increment {
            Increment::FieldUnit => Some(opentelemetry::Value::F64(1.0)),
            Increment::FieldCustom(val) => val.take(),
            Increment::Unit
            | Increment::Duration(_, _)
            | Increment::Custom(_)
            | Increment::EventDuration(_, _)
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

        if let (Some(histogram), Some(increment)) = (&inner.histogram, increment.as_ref()) {
            if let Some(value) = value_to_f64(increment) {
                histogram.record(value, &inner.attributes);
            }
            // Reset the attributes to the original length, this will discard the new attributes added from selectors.
            inner.attributes.truncate(original_length);
        }
    }
}

impl<A, T, Request, Response, EventResponse> Drop
    for CustomHistogram<Request, Response, EventResponse, A, T>
where
    A: Selectors<Request, Response, EventResponse> + Default,
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
                    Increment::Unit | Increment::EventUnit => Some(opentelemetry::Value::F64(1.0)),
                    Increment::Duration(instant, unit)
                    | Increment::EventDuration(instant, unit) => {
                        Some(duration_to_value(instant.elapsed(), unit))
                    }
                    Increment::Custom(val) | Increment::EventCustom(val) => val.clone(),
                    Increment::FieldUnit | Increment::FieldCustom(_) => {
                        // Dropping a metric on a field will never increment.
                        // We can't increment graphql metrics unless we actually process the result.
                        // It's not like we're counting the number of requests, where we want to increment
                        // with the data that we know so far if the request stops.
                        return;
                    }
                };

                if let Some(increment) = increment.as_ref()
                    && let Some(value) = value_to_f64(increment)
                {
                    histogram.record(value, &inner.attributes);
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

    use apollo_compiler::Name;
    use apollo_compiler::ast::NamedType;
    use apollo_compiler::executable::SelectionSet;
    use apollo_compiler::name;
    use apollo_federation::connectors::ConnectId;
    use apollo_federation::connectors::ConnectSpec;
    use apollo_federation::connectors::Connector;
    use apollo_federation::connectors::HTTPMethod;
    use apollo_federation::connectors::HttpJsonTransport;
    use apollo_federation::connectors::JSONSelection;
    use apollo_federation::connectors::SourceName;
    use apollo_federation::connectors::StringTemplate;
    use apollo_federation::connectors::runtime::http_json_transport::HttpRequest;
    use apollo_federation::connectors::runtime::http_json_transport::HttpResponse;
    use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
    use apollo_federation::connectors::runtime::key::ResponseKey;
    use apollo_federation::connectors::runtime::mapping::Problem;
    use apollo_federation::connectors::runtime::responses::MappedResponse;
    use http::HeaderMap;
    use http::HeaderName;
    use http::Method;
    use http::StatusCode;
    use http::Uri;
    use multimap::MultiMap;
    use rust_embed::RustEmbed;
    use schemars::generate::SchemaSettings;
    use serde::Deserialize;
    use serde_json::json;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Value;

    use super::*;
    use crate::Context;
    use crate::context::CONTAINS_GRAPHQL_ERROR;
    use crate::context::OPERATION_KIND;
    use crate::error::Error;
    use crate::graphql;
    use crate::http_ext::TryIntoHeaderName;
    use crate::http_ext::TryIntoHeaderValue;
    use crate::json_ext::Path;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::limits::operation_limits::OperationLimits;
    use crate::plugins::telemetry::APOLLO_PRIVATE_QUERY_ALIASES;
    use crate::plugins::telemetry::APOLLO_PRIVATE_QUERY_DEPTH;
    use crate::plugins::telemetry::APOLLO_PRIVATE_QUERY_HEIGHT;
    use crate::plugins::telemetry::APOLLO_PRIVATE_QUERY_ROOT_FIELDS;
    use crate::plugins::telemetry::apollo::Config;
    use crate::plugins::telemetry::config_new::cache::CacheInstruments;
    use crate::plugins::telemetry::config_new::graphql::GraphQLInstruments;
    use crate::plugins::telemetry::config_new::instruments::Instrumented;
    use crate::plugins::telemetry::config_new::instruments::InstrumentsConfig;
    use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphRequestBodySize;
    use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphResponseBodySize;
    use crate::services::OperationKind;
    use crate::services::RouterRequest;
    use crate::services::RouterResponse;
    use crate::services::connector::request_service::Request;
    use crate::services::connector::request_service::Response;
    use crate::services::connector::request_service::TransportOutcome;

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
        ConnectorRequest {
            subgraph_name: String,
            source_name: String,
            http_method: String,
            url_template: String,
            uri: String,
            #[serde(default)]
            headers: HashMap<String, String>,
            body: Option<String>,
            #[serde(default)]
            #[schemars(with = "Option<serde_json::Value>")]
            mapping_problems: Vec<Problem>,
        },
        ConnectorResponse {
            status: u16,
            #[serde(default)]
            headers: HashMap<String, String>,
            body: String,
            #[serde(default)]
            #[schemars(with = "Option<serde_json::Value>")]
            mapping_problems: Vec<Problem>,
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

    const DEFAULT_CONNECT_SPEC: ConnectSpec = ConnectSpec::V0_2;
    /// The measurements one subgraph instrument set starts, as the subgraph layer starts one per
    /// subgraph request from the set it built for its stack, are each their own request's. Two
    /// requests started from one set and run interleaved each record their own attributes, and a
    /// condition one request meets holds for it alone.
    #[tokio::test]
    async fn subgraph_requests_started_from_one_instrument_set_record_apart() {
        async {
            let mut config = load_config(
                br#"
telemetry:
  instrumentation:
    instruments:
      default_requirement_level: none
      subgraph:
        http.client.request.duration:
          attributes:
            subgraph.name: true
        custom_counter:
          description: "count of requests"
          type: counter
          unit: "unit"
          value: unit
          attributes:
            "custom_attribute":
              subgraph_request_header: "custom_header"
          condition:
            eq:
              - subgraph_request_header: "custom_header"
              - "allowed"
"#,
            );
            config.update_defaults();
            let set = config.subgraph_instrument_set(&config.new_builtin_subgraph_instruments());
            let request = |subgraph: &str, header: &'static str| {
                let mut http_request =
                    http::Request::new(graphql::Request::fake_builder().query("{ me }").build());
                http_request
                    .headers_mut()
                    .insert("custom_header", http::HeaderValue::from_static(header));
                crate::services::subgraph::Request::fake_builder()
                    .subgraph_name(subgraph)
                    .subgraph_request(http_request)
                    .build()
            };
            let response = |subgraph: &str| {
                crate::services::subgraph::Response::fake_builder()
                    .subgraph_name(subgraph)
                    .build()
            };

            let allowed = set.start();
            let refused = set.start();
            allowed.on_request(&request("products", "allowed"));
            refused.on_request(&request("reviews", "refused"));
            refused.on_response(&response("reviews"));
            allowed.on_response(&response("products"));
            drop((allowed, refused));

            assert_counter!("custom_counter", 1.0, "custom_attribute" = "allowed");
            assert_counter_not_exists!("custom_counter", f64, "custom_attribute" = "refused");
            assert_histogram_count!(
                "http.client.request.duration",
                1,
                "subgraph.name" = "products"
            );
            assert_histogram_count!(
                "http.client.request.duration",
                1,
                "subgraph.name" = "reviews"
            );
        }
        .with_metrics()
        .await;
    }

    /// A started instrument measures its duration from its request's start,
    /// not from when its set was built. A router set is built, ten seconds pass, a request starts
    /// from it, two seconds pass and it responds: the request duration it records is two seconds,
    /// where an instrument whose instant were taken with its set would record twelve. The
    /// instrument snapshots replace a duration's sum (`SerdeMetric`'s redaction of `.duration`
    /// metrics), so this reads the sum itself, under tokio's paused clock.
    #[tokio::test(start_paused = true)]
    async fn a_request_records_its_duration_from_its_own_start() {
        async {
            let mut config = load_config(
                br#"
telemetry:
  instrumentation:
    instruments:
      default_requirement_level: none
      router:
        http.server.request.duration:
          attributes:
            http.request.method: true
"#,
            );
            config.update_defaults();
            let set = config.router_instrument_set(&config.new_builtin_router_instruments());
            tokio::time::advance(std::time::Duration::from_secs(10)).await;

            let request = RouterRequest::fake_builder()
                .method(Method::GET)
                .build()
                .expect("a request");
            let instruments = set.start();
            instruments.on_request(&request);
            tokio::time::advance(std::time::Duration::from_secs(2)).await;
            instruments.on_response(
                &RouterResponse::fake_builder()
                    .context(request.context.clone())
                    .build()
                    .expect("a response"),
            );
            drop(instruments);

            assert_histogram_sum!(
                "http.server.request.duration",
                2.0,
                "http.request.method" = "GET"
            );
        }
        .with_metrics()
        .await;
    }

    /// What it protects: the measurements one router instrument set starts, as the router layer
    /// starts one per request from the set it built for its pipeline, are each their own
    /// request's. Two requests started from one set and run interleaved each record their own
    /// attributes, a condition one request meets at its request holds for it alone, each
    /// request's active count rises and falls under its own attributes, and each request
    /// stashes its own subscription-termination attributes.
    #[tokio::test]
    async fn requests_started_from_one_router_instrument_set_record_apart() {
        async {
            let mut config = load_config(
                br#"
telemetry:
  instrumentation:
    instruments:
      default_requirement_level: none
      router:
        http.server.request.duration:
          attributes:
            http.request.method: true
        http.server.active_requests:
          attributes:
            http.request.method: true
        apollo.router.operations.subscriptions.terminated.client:
          attributes:
            client.name: true
        custom_counter:
          description: "count of requests"
          type: counter
          unit: "unit"
          value: unit
          attributes:
            "custom_attribute":
              request_header: "custom_header"
          condition:
            eq:
              - request_header: "custom_header"
              - "allowed"
"#,
            );
            config.update_defaults();
            let set = config.router_instrument_set(&config.new_builtin_router_instruments());
            let request = |method: Method, header: &'static str, client: &str| {
                let context = Context::new();
                context
                    .insert(CLIENT_NAME, client.to_string())
                    .expect("the client's name");
                RouterRequest::fake_builder()
                    .context(context)
                    .method(method)
                    .header("custom_header", header)
                    .build()
                    .expect("a request")
            };
            let response = |request: &RouterRequest| {
                RouterResponse::fake_builder()
                    .context(request.context.clone())
                    .build()
                    .expect("a response")
            };
            let allowed_request = request(Method::GET, "allowed", "ios");
            let refused_request = request(Method::POST, "refused", "web");

            let allowed = set.start();
            let refused = set.start();
            allowed.on_request(&allowed_request);
            refused.on_request(&refused_request);
            assert_up_down_counter!(
                "http.server.active_requests",
                1,
                "http.request.method" = "GET"
            );
            assert_up_down_counter!(
                "http.server.active_requests",
                1,
                "http.request.method" = "POST"
            );
            refused.on_response(&response(&refused_request));
            allowed.on_response(&response(&allowed_request));
            for request in [&allowed_request, &refused_request] {
                request
                    .context
                    .extensions()
                    .with_lock(|lock| lock.get::<SubscriptionsTerminatedCounter>().cloned())
                    .expect("the request's stashed counter")
                    .record("server_close");
            }
            drop((allowed, refused));

            assert_counter!("custom_counter", 1.0, "custom_attribute" = "allowed");
            assert_counter_not_exists!("custom_counter", f64, "custom_attribute" = "refused");
            assert_histogram_count!(
                "http.server.request.duration",
                1,
                "http.request.method" = "GET"
            );
            assert_histogram_count!(
                "http.server.request.duration",
                1,
                "http.request.method" = "POST"
            );
            assert_up_down_counter!(
                "http.server.active_requests",
                0,
                "http.request.method" = "GET"
            );
            assert_up_down_counter!(
                "http.server.active_requests",
                0,
                "http.request.method" = "POST"
            );
            assert_counter!(
                "apollo.router.operations.subscriptions.terminated.client",
                1.0,
                "client.name" = "ios"
            );
            assert_counter!(
                "apollo.router.operations.subscriptions.terminated.client",
                1.0,
                "client.name" = "web"
            );
        }
        .with_metrics()
        .await;
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
                    println!("Running test for fixture: {fixture}");
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
                    let apollo_config = load_apollo_config(&router_config_file.data);
                    // the sets, built once as the layers build them for their pipelines, and
                    // started per request
                    let router_set =
                        config.router_instrument_set(&config.new_builtin_router_instruments());
                    let supergraph_set = config
                        .supergraph_instrument_set(&config.new_builtin_supergraph_instruments());
                    let graphql_set =
                        config.graphql_instrument_set(&config.new_builtin_graphql_instruments());
                    let subgraph_set =
                        config.subgraph_instrument_set(&config.new_builtin_subgraph_instruments());
                    let apollo_subgraph_set = config.apollo_subgraph_instrument_set(
                        &config.new_builtin_apollo_subgraph_instruments(),
                        apollo_config.subgraph_metrics,
                    );
                    let cache_set =
                        config.cache_instrument_set(&config.new_builtin_cache_instruments());
                    let connector_set = config
                        .connector_instrument_set(&config.new_builtin_connector_instruments());
                    let apollo_connector_set = config.apollo_connector_instrument_set(
                        &config.new_builtin_apollo_connector_instruments(),
                        apollo_config.subgraph_metrics,
                    );

                    for request in test_definition.events {
                        // each array of actions is a separate request
                        let mut router_instruments = None;
                        let mut supergraph_instruments = None;
                        let mut subgraph_instruments = None;
                        let mut subgraph_request_body_size = None;
                        let mut connector_instruments = None;
                        let mut apollo_subgraph_instruments = None;
                        let mut apollo_connector_instruments = None;
                        let mut cache_instruments: Option<CacheInstruments> = None;
                        let graphql_instruments: GraphQLInstruments = graphql_set.start();
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
                                        .body(router::body::from_bytes(body))
                                        .build()
                                        .unwrap();
                                    router_instruments = Some(router_set.start());
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
                                    supergraph_instruments = Some(supergraph_set.start());

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
                                    subgraph_instruments = Some(subgraph_set.start());
                                    apollo_subgraph_instruments = Some(apollo_subgraph_set.start());
                                    cache_instruments = Some(cache_set.start());
                                    let graphql_request = graphql::Request::fake_builder()
                                        .query(query)
                                        .and_operation_name(operation_name)
                                        .variables(variables)
                                        .extensions(extensions)
                                        .build();
                                    let mut http_request = http::Request::new(graphql_request);
                                    *http_request.headers_mut() = convert_http_headers(headers);

                                    let request =
                                        crate::services::subgraph::Request::fake_builder()
                                            .context(context.clone())
                                            .subgraph_name(subgraph_name)
                                            .and_operation_kind(operation_kind)
                                            .subgraph_request(http_request)
                                            .build();

                                    let body =
                                        serde_json::to_string(request.subgraph_request.body())
                                            .expect("failed to serialize subgraph request body");
                                    // The subgraph service places the request's body size on
                                    // its response.
                                    subgraph_request_body_size =
                                        Some(SubgraphRequestBodySize(body.len() as u64));

                                    subgraph_instruments.as_mut().unwrap().on_request(&request);
                                    apollo_subgraph_instruments
                                        .as_mut()
                                        .unwrap()
                                        .on_request(&request);
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
                                    let mut response =
                                        crate::services::subgraph::Response::fake2_builder()
                                            .context(context.clone())
                                            .and_subgraph_name(subgraph_name)
                                            .status_code(
                                                StatusCode::from_u16(status).expect("status"),
                                            )
                                            .and_data(data)
                                            .errors(errors)
                                            .extensions(extensions)
                                            .headers(convert_headers(headers))
                                            .build()
                                            .unwrap();

                                    let body = serde_json::to_string(response.response.body())
                                        .expect("failed to serialize subgraph response body");
                                    let extensions = response.response.extensions_mut();
                                    extensions.insert(SubgraphResponseBodySize(body.len() as u64));
                                    if let Some(size) = subgraph_request_body_size.take() {
                                        extensions.insert(size);
                                    }

                                    subgraph_instruments
                                        .take()
                                        .expect("subgraph request must have been made first")
                                        .on_response(&response);
                                    apollo_subgraph_instruments
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
                                            context.extensions().with_lock(|lock| {
                                                let limits = lock
                                                    .get_or_default_mut::<OperationLimits<u32>>();
                                                let value_as_u32 = value.as_u64().unwrap() as u32;
                                                limits.aliases = value_as_u32;
                                            });
                                        }
                                        if key == APOLLO_PRIVATE_QUERY_DEPTH.to_string() {
                                            context.extensions().with_lock(|lock| {
                                                let limits = lock
                                                    .get_or_default_mut::<OperationLimits<u32>>();
                                                let value_as_u32 = value.as_u64().unwrap() as u32;
                                                limits.depth = value_as_u32;
                                            });
                                        }
                                        if key == APOLLO_PRIVATE_QUERY_HEIGHT.to_string() {
                                            context.extensions().with_lock(|lock| {
                                                let limits = lock
                                                    .get_or_default_mut::<OperationLimits<u32>>();
                                                let value_as_u32 = value.as_u64().unwrap() as u32;
                                                limits.height = value_as_u32;
                                            });
                                        }
                                        if key == APOLLO_PRIVATE_QUERY_ROOT_FIELDS.to_string() {
                                            context.extensions().with_lock(|lock| {
                                                let limits = lock
                                                    .get_or_default_mut::<OperationLimits<u32>>();
                                                let value_as_u32 = value.as_u64().unwrap() as u32;
                                                limits.root_fields = value_as_u32;
                                            });
                                        }
                                    }
                                }
                                Event::ConnectorRequest {
                                    subgraph_name,
                                    source_name,
                                    http_method,
                                    url_template,
                                    uri,
                                    headers,
                                    body,
                                    mapping_problems,
                                } => {
                                    let mut http_request = http::Request::builder()
                                        .method(Method::from_str(&http_method).expect("method"))
                                        .uri(Uri::from_str(&uri).expect("uri"))
                                        .body(body.unwrap_or("".into()))
                                        .unwrap();
                                    *http_request.headers_mut() = convert_http_headers(headers);
                                    let transport_request =
                                        TransportRequest::Http(Box::new(HttpRequest {
                                            inner: http_request,
                                            debug: Default::default(),
                                        }));
                                    let connector = Connector {
                                        id: ConnectId::new(
                                            subgraph_name,
                                            Some(SourceName::cast(&source_name)),
                                            name!(Query),
                                            name!(field),
                                            None,
                                            0,
                                        ),
                                        transport: Some(HttpJsonTransport {
                                            connect_template: StringTemplate::parse_with_spec(
                                                url_template.as_str(),
                                                DEFAULT_CONNECT_SPEC,
                                            )
                                            .unwrap(),
                                            method: HTTPMethod::from_str(http_method.as_str())
                                                .unwrap(),
                                            ..Default::default()
                                        }),
                                        selection: JSONSelection::empty(),
                                        config: None,
                                        max_requests: None,
                                        entity_resolver: None,
                                        spec: DEFAULT_CONNECT_SPEC,
                                        schema_subtypes_map: Default::default(),
                                        batch_settings: None,
                                        request_headers: Default::default(),
                                        response_headers: Default::default(),
                                        request_variable_keys: Default::default(),
                                        response_variable_keys: Default::default(),
                                        error_settings: Default::default(),
                                        output_type: None,
                                        label: "label".into(),
                                    };
                                    let response_key = ResponseKey::RootField {
                                        name: "hello".to_string(),
                                        inputs: Default::default(),
                                        selection: Arc::new(
                                            JSONSelection::parse_with_spec(
                                                "$.data",
                                                DEFAULT_CONNECT_SPEC,
                                            )
                                            .unwrap(),
                                        ),
                                    };
                                    let request = Request {
                                        context: context.clone(),
                                        connector: Arc::new(connector),
                                        transport_request,
                                        key: response_key.clone(),
                                        mapping_problems,
                                        supergraph_request: Default::default(),
                                        operation: Default::default(),
                                    };
                                    connector_instruments = Some({
                                        let connector_instruments = connector_set.start();
                                        connector_instruments.on_request(&request);
                                        connector_instruments
                                    });
                                    apollo_connector_instruments = Some({
                                        let apollo_connector_instruments =
                                            apollo_connector_set.start();
                                        apollo_connector_instruments.on_request(&request);
                                        apollo_connector_instruments
                                    })
                                }
                                Event::ConnectorResponse {
                                    status,
                                    headers,
                                    body,
                                    mapping_problems,
                                    ..
                                } => {
                                    let response_key = ResponseKey::RootField {
                                        name: "hello".to_string(),
                                        inputs: Default::default(),
                                        selection: Arc::new(
                                            JSONSelection::parse_with_spec(
                                                "$.data",
                                                DEFAULT_CONNECT_SPEC,
                                            )
                                            .unwrap(),
                                        ),
                                    };
                                    let mut http_response = http::Response::builder()
                                        .status(StatusCode::from_u16(status).expect("status"))
                                        .body(router::body::from_bytes(body))
                                        .unwrap();
                                    *http_response.headers_mut() = convert_http_headers(headers);
                                    let response = Response {
                                        context: context.clone(),
                                        subgraph_name: String::new(),
                                        transport_outcome: TransportOutcome::Response(
                                            HttpResponse {
                                                inner: http_response.into_parts().0,
                                            },
                                        ),
                                        mapped_response: MappedResponse::Data {
                                            data: json!({})
                                                .try_into()
                                                .expect("expecting valid JSON"),
                                            key: response_key,
                                            problems: mapping_problems,
                                            declared_errors: vec![],
                                        },
                                        answered_by_router: false,
                                    };
                                    connector_instruments
                                        .take()
                                        .expect("connector request must have been made first")
                                        .on_response(&response);
                                    apollo_connector_instruments
                                        .take()
                                        .expect("connector request must have been made first")
                                        .on_response(&response);
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

    fn load_apollo_config(config: &[u8]) -> Config {
        let val: serde_json::Value = serde_yaml::from_slice(config).unwrap();
        let apollo_config = &val["telemetry"]["apollo"];
        serde_json::from_value(apollo_config.clone()).unwrap_or_default()
    }

    #[test]
    fn write_schema() {
        // Write a json schema for the above test
        let mut schema_gen = SchemaSettings::draft07().into_generator();
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

            let router_instruments = config
                .router_instrument_set(&config.new_builtin_router_instruments())
                .start();
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
                40.0,
                "acme.my_attribute" = "TEST"
            );

            let router_instruments = config
                .router_instrument_set(&config.new_builtin_router_instruments())
                .start();
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
                40.0,
                "acme.my_attribute" = "TEST"
            );
            assert_histogram_sum!(
                "http.server.response.body.size",
                40.0,
                "acme.my_attribute" = "unknown"
            );

            let router_instruments = config
                .router_instrument_set(&config.new_builtin_router_instruments())
                .start();
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

            let router_instruments = config
                .router_instrument_set(&config.new_builtin_router_instruments())
                .start();
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

            let custom_instruments = CustomInstrumentSet::new(
                &config.supergraph.custom,
                &config.new_builtin_supergraph_instruments(),
            )
            .start();
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
                .errors(vec![
                    graphql::Error::builder()
                        .message("nope")
                        .extension_code("NOPE")
                        .build(),
                ])
                .build()
                .unwrap();
            custom_instruments.on_response(&supergraph_response);
            custom_instruments.on_response_event(
                &graphql::Response::builder()
                    .data(json!({
                        "price": 500
                    }))
                    .errors(vec![
                        graphql::Error::builder()
                            .message("nope")
                            .extension_code("NOPE")
                            .build(),
                    ])
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

            let custom_instruments = CustomInstrumentSet::new(
                &config.supergraph.custom,
                &config.new_builtin_supergraph_instruments(),
            )
            .start();
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
                .errors(vec![
                    graphql::Error::builder()
                        .message("nope")
                        .extension_code("NOPE")
                        .build(),
                ])
                .build()
                .unwrap();
            custom_instruments.on_response(&supergraph_response);
            custom_instruments.on_response_event(
                &graphql::Response::builder()
                    .data(json!({
                        "price": 500
                    }))
                    .errors(vec![
                        graphql::Error::builder()
                            .message("nope")
                            .extension_code("NOPE")
                            .build(),
                    ])
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

            let custom_instruments = CustomInstrumentSet::new(
                &config.supergraph.custom,
                &config.new_builtin_supergraph_instruments(),
            )
            .start();
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

    #[test]
    fn test_duration_to_f64_seconds() {
        let duration = std::time::Duration::from_secs(2);
        assert_eq!(duration_to_f64(duration, &DurationUnit::named("s")), 2.0);
        assert_eq!(duration_to_f64(duration, &DurationUnit::named("")), 2.0); // empty unit defaults to seconds
        assert_eq!(
            duration_to_f64(duration, &DurationUnit::named("unknown")),
            2.0
        ); // unknown unit defaults to seconds
    }

    #[test]
    fn test_duration_to_f64_milliseconds() {
        let duration = std::time::Duration::from_millis(1500);
        assert_eq!(
            duration_to_f64(duration, &DurationUnit::named("ms")),
            1500.0
        );
    }

    #[test]
    fn test_duration_to_f64_microseconds() {
        let duration = std::time::Duration::from_micros(500);
        assert_eq!(duration_to_f64(duration, &DurationUnit::named("us")), 500.0);
    }

    #[test]
    fn test_duration_to_f64_nanoseconds() {
        let duration = std::time::Duration::from_nanos(1234567);
        assert_eq!(
            duration_to_f64(duration, &DurationUnit::named("ns")),
            1234567.0
        );
    }

    #[test]
    fn test_duration_to_f64_fractional_seconds() {
        let duration = std::time::Duration::from_millis(1500);
        // When unit is "s", 1500ms should be 1.5 seconds
        assert_eq!(duration_to_f64(duration, &DurationUnit::named("s")), 1.5);
    }

    #[test]
    fn test_duration_to_f64_fractional_milliseconds() {
        let duration = std::time::Duration::from_micros(1500);
        // When unit is "ms", 1500us should be 1.5 milliseconds
        assert_eq!(duration_to_f64(duration, &DurationUnit::named("ms")), 1.5);
    }

    /// Verify that the `RouterInstruments::on_response` dispatch to `subscriptions_terminated`
    /// is wired up end-to-end. If the delegation were removed or mis-wired, `stashed_attributes`
    /// would be empty and the `subgraph.name`/`client.name` labels would silently disappear from
    /// the metric.
    #[tokio::test]
    async fn test_router_instruments_on_response_wires_subscriptions_terminated() {
        async {
            let config: InstrumentsConfig = serde_json::from_str(
                json!({
                    "router": {
                        "apollo.router.operations.subscriptions.terminated.client": {
                            "attributes": {
                                "reason": true,
                                "subgraph.name": true,
                                "client.name": true
                            }
                        }
                    }
                })
                .to_string()
                .as_str(),
            )
            .unwrap();

            let router_instruments = config
                .router_instrument_set(&config.new_builtin_router_instruments())
                .start();

            let ctx = Context::new();
            ctx.insert(
                SUBSCRIPTION_SUBGRAPH_NAME_CONTEXT_KEY,
                "products".to_string(),
            )
            .unwrap();
            ctx.insert(CLIENT_NAME, "ios-client".to_string()).unwrap();

            let router_req = RouterRequest::fake_builder()
                .context(ctx.clone())
                .build()
                .unwrap();
            router_instruments.on_request(&router_req);

            let router_response = RouterResponse::fake_builder()
                .context(ctx.clone())
                .build()
                .unwrap();
            router_instruments.on_response(&router_response);

            // Extract the counter stashed into context extensions by on_request, which now
            // carries the attributes populated by on_response via the shared Arc.
            let counter = ctx
                .extensions()
                .with_lock(|lock| lock.get::<SubscriptionsTerminatedCounter>().cloned())
                .expect("counter must be present after on_request");

            counter.record("server_close");

            assert_counter!(
                "apollo.router.operations.subscriptions.terminated.client",
                1,
                "reason" = "server_close",
                "subgraph.name" = "products",
                "client.name" = "ios-client"
            );
        }
        .with_metrics()
        .await;
    }
}
