use std::collections::HashMap;
use std::sync::Arc;

use opentelemetry::metrics::MeterProvider;
use tokio::time::Instant;
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
use crate::plugins::telemetry::apollo::Config;
use crate::plugins::telemetry::config_new::attributes::StandardAttribute;
use crate::plugins::telemetry::config_new::connector::ConnectorRequest;
use crate::plugins::telemetry::config_new::connector::ConnectorResponse;
use crate::plugins::telemetry::config_new::connector::attributes::ConnectorAttributes;
use crate::plugins::telemetry::config_new::connector::selectors::ConnectorSelector;
use crate::plugins::telemetry::config_new::connector::selectors::ConnectorSource::Name;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::APOLLO_ROUTER_OPERATIONS_FETCH_DURATION;
use crate::plugins::telemetry::config_new::instruments::CustomHistogram;
use crate::plugins::telemetry::config_new::instruments::Increment;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::instruments::METER_NAME;
use crate::plugins::telemetry::config_new::instruments::StaticInstrument;
use crate::plugins::telemetry::config_new::selectors::OperationKind;
use crate::plugins::telemetry::config_new::selectors::OperationName;
use crate::plugins::telemetry::config_new::subgraph::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphSelector;
use crate::query_planner::APOLLO_OPERATION_ID;
use crate::services::subgraph;

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

impl ApolloSubgraphInstruments {
    pub(crate) fn new(
        static_instruments: Arc<HashMap<String, StaticInstrument>>,
        apollo_config: Config,
    ) -> Self {
        let selectors = Extendable {
            attributes: SubgraphAttributes::builder()
                .subgraph_name(StandardAttribute::Bool(true))
                .build(),
            custom: HashMap::from([
                (
                    APOLLO_CLIENT_NAME_ATTRIBUTE.to_string(),
                    SubgraphSelector::ResponseContext {
                        response_context: CLIENT_NAME.to_string(),
                        redact: None,
                        default: None,
                    },
                ),
                (
                    APOLLO_CLIENT_VERSION_ATTRIBUTE.to_string(),
                    SubgraphSelector::ResponseContext {
                        response_context: CLIENT_VERSION.to_string(),
                        redact: None,
                        default: None,
                    },
                ),
                (
                    GRAPHQL_OPERATION_NAME_ATTRIBUTE.to_string(),
                    SubgraphSelector::SupergraphOperationName {
                        supergraph_operation_name: OperationName::String,
                        redact: None,
                        default: None,
                    },
                ),
                (
                    GRAPHQL_OPERATION_TYPE_ATTRIBUTE.to_string(),
                    SubgraphSelector::SupergraphOperationKind {
                        supergraph_operation_kind: OperationKind::String,
                    },
                ),
                (
                    APOLLO_OPERATION_ID_ATTRIBUTE.to_string(),
                    SubgraphSelector::ResponseContext {
                        response_context: APOLLO_OPERATION_ID.to_string(),
                        redact: None,
                        default: None,
                    },
                ),
                (
                    APOLLO_HAS_ERRORS_ATTRIBUTE.to_string(),
                    SubgraphSelector::OnGraphQLError {
                        subgraph_on_graphql_error: true,
                    },
                ),
            ]),
        };
        let attribute_count = selectors.custom.len() + 1; // 1 for subgraph_name on attributes

        let apollo_router_operations_fetch_duration =
            apollo_config.preview_subgraph_metrics.then(|| {
                CustomHistogram::builder()
                    .increment(Increment::Duration(Instant::now()))
                    .attributes(Vec::with_capacity(attribute_count))
                    .selectors(Arc::new(selectors))
                    .histogram(static_instruments
                        .get(APOLLO_ROUTER_OPERATIONS_FETCH_DURATION)
                        .expect(
                            "cannot get apollo static instrument for subgraph; this should not happen",
                        )
                        .as_histogram()
                        .cloned()
                        .expect(
                            "cannot convert apollo instrument to histogram for subgraph; this should not happen",
                        )
                    )
                    .build()
            });

        Self {
            apollo_router_operations_fetch_duration,
        }
    }

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

impl ApolloConnectorInstruments {
    pub(crate) fn new(
        static_instruments: Arc<HashMap<String, StaticInstrument>>,
        apollo_config: Config,
    ) -> Self {
        let selectors = Extendable {
            attributes: ConnectorAttributes::builder()
                .subgraph_name(StandardAttribute::Bool(true))
                .build(),
            custom: HashMap::from([
                (
                    APOLLO_CLIENT_NAME_ATTRIBUTE.to_string(),
                    ConnectorSelector::RequestContext {
                        request_context: CLIENT_NAME.to_string(),
                        redact: None,
                        default: None,
                    },
                ),
                (
                    APOLLO_CLIENT_VERSION_ATTRIBUTE.to_string(),
                    ConnectorSelector::RequestContext {
                        request_context: CLIENT_VERSION.to_string(),
                        redact: None,
                        default: None,
                    },
                ),
                (
                    GRAPHQL_OPERATION_NAME_ATTRIBUTE.to_string(),
                    ConnectorSelector::SupergraphOperationName {
                        supergraph_operation_name: OperationName::String,
                        redact: None,
                        default: None,
                    },
                ),
                (
                    GRAPHQL_OPERATION_TYPE_ATTRIBUTE.to_string(),
                    ConnectorSelector::SupergraphOperationKind {
                        supergraph_operation_kind: OperationKind::String,
                    },
                ),
                (
                    APOLLO_OPERATION_ID_ATTRIBUTE.to_string(),
                    ConnectorSelector::RequestContext {
                        request_context: APOLLO_OPERATION_ID.to_string(),
                        redact: None,
                        default: None,
                    },
                ),
                (
                    APOLLO_HAS_ERRORS_ATTRIBUTE.to_string(),
                    ConnectorSelector::OnResponseError {
                        connector_on_response_error: true,
                    },
                ),
                (
                    APOLLO_CONNECTOR_SOURCE_ATTRIBUTE.to_string(),
                    ConnectorSelector::ConnectorSource {
                        connector_source: Name,
                    },
                ),
            ]),
        };
        let attribute_count = selectors.custom.len() + 1; // 1 for subgraph_name on attributes

        let apollo_router_operations_fetch_duration =
            apollo_config.preview_subgraph_metrics.then(|| {
                CustomHistogram::builder()
                    .increment(Increment::Duration(Instant::now()))
                    .attributes(Vec::with_capacity(attribute_count))
                    .selectors(Arc::new(selectors))
                    .histogram(static_instruments
                        .get(APOLLO_ROUTER_OPERATIONS_FETCH_DURATION)
                        .expect(
                            "cannot get apollo static instrument for subgraph; this should not happen",
                        )
                        .as_histogram()
                        .cloned()
                        .expect(
                            "cannot convert apollo instrument to histogram for subgraph; this should not happen",
                        )
                    )
                    .build()
            });

        Self {
            apollo_router_operations_fetch_duration,
        }
    }

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
