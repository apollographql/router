use tower::BoxError;

use crate::Context;
use crate::plugins::telemetry::config_new::instruments::CustomHistogram;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::subgraph::attributes::SubgraphAttributes;
use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphSelector;
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
