use std::collections::HashMap;
use std::sync::Arc;

use opentelemetry::metrics::MeterProvider;
use tower::BoxError;

use crate::Context;
use crate::metrics;
use crate::plugins::telemetry::APOLLO_CLIENT_NAME_ATTRIBUTE;
use crate::plugins::telemetry::APOLLO_CLIENT_VERSION_ATTRIBUTE;
use crate::plugins::telemetry::APOLLO_CONNECTOR_SOURCE_ATTRIBUTE;
use crate::plugins::telemetry::APOLLO_HAS_ERRORS_ATTRIBUTE;
use crate::plugins::telemetry::APOLLO_OPERATION_ID_ATTRIBUTE;
use crate::plugins::telemetry::CLIENT_NAME;
use crate::plugins::telemetry::CLIENT_VERSION;
use crate::plugins::telemetry::GRAPHQL_OPERATION_NAME_ATTRIBUTE;
use crate::plugins::telemetry::GRAPHQL_OPERATION_TYPE_ATTRIBUTE;
use crate::plugins::telemetry::config_new::attributes::StandardAttribute;
use crate::plugins::telemetry::config_new::connector::ConnectorRequest;
use crate::plugins::telemetry::config_new::connector::ConnectorResponse;
use crate::plugins::telemetry::config_new::connector::attributes::ConnectorAttributes;
use crate::plugins::telemetry::config_new::connector::selectors::ConnectorSelector;
use crate::plugins::telemetry::config_new::connector::selectors::ConnectorSource::Name;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::APOLLO_ROUTER_OPERATIONS_FETCH_DURATION;
use crate::plugins::telemetry::config_new::instruments::CustomHistogram;
use crate::plugins::telemetry::config_new::instruments::DurationUnit;
use crate::plugins::telemetry::config_new::instruments::HistogramInstrument;
use crate::plugins::telemetry::config_new::instruments::IncrementKind;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::instruments::METER_NAME;
use crate::plugins::telemetry::config_new::instruments::StaticInstrument;
use crate::plugins::telemetry::config_new::selectors::OperationKind;
use crate::plugins::telemetry::config_new::selectors::OperationName;
use crate::plugins::telemetry::config_new::subgraph::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphSelector;
use crate::query_planner::APOLLO_OPERATION_ID;
use crate::services::subgraph;

/// Apollo's subgraph instrument, `apollo.router.operations.fetch.duration`, as one subgraph
/// request records into it ([`ApolloSubgraphInstrumentSet::start`]).
pub(crate) struct ApolloSubgraphInstruments {
    pub(crate) apollo_router_operations_fetch_duration: Option<
        CustomHistogram<
            subgraph::Request,
            subgraph::Response,
            (),
            SubgraphAttributes,
            SubgraphSelector,
        >,
    >,
}

pub(crate) struct ApolloConnectorInstruments {
    pub(crate) apollo_router_operations_fetch_duration: Option<
        CustomHistogram<
            ConnectorRequest,
            ConnectorResponse,
            (),
            ConnectorAttributes,
            ConnectorSelector,
        >,
    >,
}

/// Apollo's subgraph instrument, built once for a pipeline: its selectors, keyed by the
/// attributes they make, and its histogram. [`Self::start`] makes the instrument one subgraph request
/// records into.
pub(crate) struct ApolloSubgraphInstrumentSet {
    apollo_router_operations_fetch_duration:
        Option<HistogramInstrument<SubgraphAttributes, SubgraphSelector>>,
}

impl ApolloSubgraphInstrumentSet {
    /// The instrument over `static_instruments`, where `subgraph_metrics`.
    pub(crate) fn new(
        static_instruments: &HashMap<String, StaticInstrument>,
        subgraph_metrics: bool,
    ) -> Self {
        let apollo_router_operations_fetch_duration = subgraph_metrics.then(|| {
            let selectors = Extendable {
                attributes: SubgraphAttributes::builder()
                    .subgraph_name(StandardAttribute::Bool(true))
                    .build(),
                custom: HashMap::from([
                    (
                        APOLLO_CLIENT_NAME_ATTRIBUTE.into(),
                        SubgraphSelector::ResponseContext {
                            response_context: CLIENT_NAME.to_string(),
                            default: None,
                        },
                    ),
                    (
                        APOLLO_CLIENT_VERSION_ATTRIBUTE.into(),
                        SubgraphSelector::ResponseContext {
                            response_context: CLIENT_VERSION.to_string(),
                            default: None,
                        },
                    ),
                    (
                        GRAPHQL_OPERATION_NAME_ATTRIBUTE.into(),
                        SubgraphSelector::SupergraphOperationName {
                            supergraph_operation_name: OperationName::String,
                            default: None,
                        },
                    ),
                    (
                        GRAPHQL_OPERATION_TYPE_ATTRIBUTE.into(),
                        SubgraphSelector::SupergraphOperationKind {
                            supergraph_operation_kind: OperationKind::String,
                        },
                    ),
                    (
                        APOLLO_OPERATION_ID_ATTRIBUTE.into(),
                        SubgraphSelector::ResponseContext {
                            response_context: APOLLO_OPERATION_ID.to_string(),
                            default: None,
                        },
                    ),
                    (
                        APOLLO_HAS_ERRORS_ATTRIBUTE.into(),
                        SubgraphSelector::OnGraphQLError {
                            subgraph_on_graphql_error: true,
                        },
                    ),
                ]),
            };
            HistogramInstrument::new(
                IncrementKind::Duration(DurationUnit::Seconds),
                static_instruments
                    .get(APOLLO_ROUTER_OPERATIONS_FETCH_DURATION)
                    .expect(
                        "cannot get apollo static instrument for subgraph; this should not happen",
                    )
                    .as_histogram()
                    .cloned()
                    .expect(
                        "cannot convert apollo instrument to histogram for subgraph; this should not happen",
                    ),
                None,
                Some(Arc::new(selectors)),
            )
        });
        Self {
            apollo_router_operations_fetch_duration,
        }
    }

    /// The instrument one subgraph request records into.
    pub(crate) fn start(&self) -> ApolloSubgraphInstruments {
        ApolloSubgraphInstruments {
            apollo_router_operations_fetch_duration: self
                .apollo_router_operations_fetch_duration
                .as_ref()
                .map(HistogramInstrument::start),
        }
    }
}

impl ApolloSubgraphInstruments {
    pub(crate) fn new_builtin() -> HashMap<String, StaticInstrument> {
        create_subgraph_and_connector_shared_static_instruments()
    }
}

impl Instrumented for ApolloSubgraphInstruments {
    type Request = subgraph::Request;
    type Response = subgraph::Response;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        if let Some(apollo_router_operations_fetch_duration) =
            &self.apollo_router_operations_fetch_duration
        {
            apollo_router_operations_fetch_duration.on_request(request);
        }
    }

    fn on_response(&self, response: &Self::Response) {
        if let Some(apollo_router_operations_fetch_duration) =
            &self.apollo_router_operations_fetch_duration
        {
            apollo_router_operations_fetch_duration.on_response(response);
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        if let Some(apollo_router_operations_fetch_duration) =
            &self.apollo_router_operations_fetch_duration
        {
            apollo_router_operations_fetch_duration.on_error(error, ctx);
        }
    }
}

/// Apollo's connector instrument, built once for a pipeline: its selectors, keyed by the
/// attributes they make, and its histogram. [`Self::start`] makes the instrument one connector request
/// records into.
pub(crate) struct ApolloConnectorInstrumentSet {
    apollo_router_operations_fetch_duration:
        Option<HistogramInstrument<ConnectorAttributes, ConnectorSelector>>,
}

impl ApolloConnectorInstrumentSet {
    /// The instrument over `static_instruments`, where `subgraph_metrics`.
    pub(crate) fn new(
        static_instruments: &HashMap<String, StaticInstrument>,
        subgraph_metrics: bool,
    ) -> Self {
        let apollo_router_operations_fetch_duration = subgraph_metrics.then(|| {
            let selectors = Extendable {
                attributes: ConnectorAttributes::builder()
                    .subgraph_name(StandardAttribute::Bool(true))
                    .build(),
                custom: HashMap::from([
                    (
                        APOLLO_CLIENT_NAME_ATTRIBUTE.into(),
                        ConnectorSelector::RequestContext {
                            request_context: CLIENT_NAME.to_string(),
                            default: None,
                        },
                    ),
                    (
                        APOLLO_CLIENT_VERSION_ATTRIBUTE.into(),
                        ConnectorSelector::RequestContext {
                            request_context: CLIENT_VERSION.to_string(),
                            default: None,
                        },
                    ),
                    (
                        GRAPHQL_OPERATION_NAME_ATTRIBUTE.into(),
                        ConnectorSelector::SupergraphOperationName {
                            supergraph_operation_name: OperationName::String,
                            default: None,
                        },
                    ),
                    (
                        GRAPHQL_OPERATION_TYPE_ATTRIBUTE.into(),
                        ConnectorSelector::SupergraphOperationKind {
                            supergraph_operation_kind: OperationKind::String,
                        },
                    ),
                    (
                        APOLLO_OPERATION_ID_ATTRIBUTE.into(),
                        ConnectorSelector::RequestContext {
                            request_context: APOLLO_OPERATION_ID.to_string(),
                            default: None,
                        },
                    ),
                    (
                        APOLLO_HAS_ERRORS_ATTRIBUTE.into(),
                        ConnectorSelector::OnResponseError {
                            connector_on_response_error: true,
                        },
                    ),
                    (
                        APOLLO_CONNECTOR_SOURCE_ATTRIBUTE.into(),
                        ConnectorSelector::ConnectorSource {
                            connector_source: Name,
                        },
                    ),
                ]),
            };
            HistogramInstrument::new(
                IncrementKind::Duration(DurationUnit::Seconds),
                static_instruments
                    .get(APOLLO_ROUTER_OPERATIONS_FETCH_DURATION)
                    .expect(
                        "cannot get apollo static instrument for connector; this should not happen",
                    )
                    .as_histogram()
                    .cloned()
                    .expect(
                        "cannot convert apollo instrument to histogram for connector; this should not happen",
                    ),
                None,
                Some(Arc::new(selectors)),
            )
        });
        Self {
            apollo_router_operations_fetch_duration,
        }
    }

    /// The instrument one connector request records into.
    pub(crate) fn start(&self) -> ApolloConnectorInstruments {
        ApolloConnectorInstruments {
            apollo_router_operations_fetch_duration: self
                .apollo_router_operations_fetch_duration
                .as_ref()
                .map(HistogramInstrument::start),
        }
    }
}

impl ApolloConnectorInstruments {
    pub(crate) fn new_builtin() -> HashMap<String, StaticInstrument> {
        create_subgraph_and_connector_shared_static_instruments()
    }
}

impl Instrumented for ApolloConnectorInstruments {
    type Request = ConnectorRequest;
    type Response = ConnectorResponse;
    type EventResponse = ();

    fn on_request(&self, request: &Self::Request) {
        if let Some(apollo_router_operations_fetch_duration) =
            &self.apollo_router_operations_fetch_duration
        {
            apollo_router_operations_fetch_duration.on_request(request);
        }
    }

    fn on_response(&self, response: &Self::Response) {
        if let Some(apollo_router_operations_fetch_duration) =
            &self.apollo_router_operations_fetch_duration
        {
            apollo_router_operations_fetch_duration.on_response(response);
        }
    }

    fn on_error(&self, error: &BoxError, ctx: &Context) {
        if let Some(apollo_router_operations_fetch_duration) =
            &self.apollo_router_operations_fetch_duration
        {
            apollo_router_operations_fetch_duration.on_error(error, ctx);
        }
    }
}

fn create_subgraph_and_connector_shared_static_instruments() -> HashMap<String, StaticInstrument> {
    let meter = metrics::meter_provider().meter(METER_NAME);
    let mut static_instruments = HashMap::with_capacity(1);
    static_instruments.insert(
        APOLLO_ROUTER_OPERATIONS_FETCH_DURATION.to_string(),
        StaticInstrument::Histogram(
            meter
                .f64_histogram(APOLLO_ROUTER_OPERATIONS_FETCH_DURATION)
                .with_unit("s")
                .with_description("Duration of a subgraph fetch.")
                .build(),
        ),
    );
    static_instruments
}
