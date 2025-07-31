use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

use opentelemetry::metrics::MeterProvider;
use parking_lot::Mutex;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::handshake::server::Callback;
use tower::BoxError;
use tower_http::trace::OnResponse;

use crate::Context;
use crate::metrics;
use crate::plugins::telemetry::CLIENT_NAME;
use crate::plugins::telemetry::CLIENT_VERSION;
use crate::plugins::telemetry::apollo::Config;
use crate::plugins::telemetry::config_new::attributes::StandardAttribute;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::instruments::APOLLO_ROUTER_OPERATIONS_FETCH_DURATION;
use crate::plugins::telemetry::config_new::instruments::CustomHistogram;
use crate::plugins::telemetry::config_new::instruments::CustomHistogramInner;
use crate::plugins::telemetry::config_new::instruments::Increment;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::instruments::METER_NAME;
use crate::plugins::telemetry::config_new::instruments::StaticInstrument;
use crate::plugins::telemetry::config_new::selectors::{OperationKind, OperationName};
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
                    "client.name".to_string(),
                    SubgraphSelector::ResponseContext {
                        response_context: CLIENT_NAME.to_string(),
                        redact: None,
                        default: None,
                    },
                ),
                (
                    "client.version".to_string(),
                    SubgraphSelector::ResponseContext {
                        response_context: CLIENT_VERSION.to_string(),
                        redact: None,
                        default: None,
                    },
                ),
                (
                    "graphql.operation.name".to_string(),
                    SubgraphSelector::SupergraphOperationName {
                        supergraph_operation_name: OperationName::String,
                        redact: None,
                        default: None,
                    },
                ),
                (
                    "graphql.operation.type".to_string(),
                    SubgraphSelector::SupergraphOperationKind {
                        supergraph_operation_kind: OperationKind::String,
                    },
                ),
                (
                    "operation.id".to_string(),
                    SubgraphSelector::ResponseContext {
                        response_context: APOLLO_OPERATION_ID.to_string(),
                        redact: None,
                        default: None,
                    },
                ),
                (
                    "has.errors".to_string(),
                    SubgraphSelector::OnGraphQLError {
                        subgraph_on_graphql_error: true,
                    },
                ),
            ]),
        };

        let apollo_router_operations_fetch_duration =
            apollo_config.experimental_subgraph_metrics.then(|| {
                CustomHistogram {
                    inner: Mutex::new(
                        CustomHistogramInner {
                            increment: Increment::Duration(Instant::now()),
                            condition: Condition::True,
                            attributes: Vec::with_capacity(7),
                            selector: None,
                            selectors: Some(
                                Arc::new(
                                    selectors,
                                )
                            ),
                            histogram: Some(static_instruments
                                .get(APOLLO_ROUTER_OPERATIONS_FETCH_DURATION)
                                .expect(
                                    "cannot get apollo static instrument for subgraph; this should not happen",
                                )
                                .as_histogram()
                                .cloned()
                                .expect(
                                    "cannot convert apollo instrument to histogram for subgraph; this should not happen",
                                )
                            ),
                            updated: false,
                            _phantom: PhantomData,
                        })
                }
            });

        Self {
            apollo_router_operations_fetch_duration,
        }
    }

    pub(crate) fn new_builtin() -> HashMap<String, StaticInstrument> {
        let meter = metrics::meter_provider().meter(METER_NAME);
        let mut static_instruments = HashMap::with_capacity(1);

        static_instruments.insert(
            APOLLO_ROUTER_OPERATIONS_FETCH_DURATION.to_string(),
            StaticInstrument::Histogram(
                meter
                    .f64_histogram(APOLLO_ROUTER_OPERATIONS_FETCH_DURATION)
                    .with_unit("s")
                    .with_description("Duration of a subgraph fetch.")
                    .init(),
            ),
        );

        static_instruments
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
