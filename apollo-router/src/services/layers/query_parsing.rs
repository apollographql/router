//!  (A)utomatic (P)ersisted (Q)ueries cache.
//!
//!  For more information on QueryParsing see:
//!  <https://www.apollographql.com/docs/apollo-server/performance/QueryParsing/>

// This entire file is license key functionality
use std::sync::Arc;

use tracing_futures::Instrument;

use crate::error::QueryPlannerError;
use crate::graphql;
use crate::query_planner::QueryKey;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::spec::Query;
use crate::spec::Schema;
use crate::Configuration;
use crate::Context;

pub(crate) struct PartialSupergraphRequest {
    /// Original request to the Router.
    pub(crate) supergraph_request: http::Request<graphql::Request>,

    /// Context for extension
    pub(crate) context: Context,
}

/// [`Layer`] for QueryParsing implementation.
#[derive(Clone)]
pub(crate) struct QueryParsingLayer {
    /// set to None if QueryParsing is disabled
    schema: Arc<Schema>,
    configuration: Arc<Configuration>,
}

impl QueryParsingLayer {
    pub(crate) fn new(schema: Arc<Schema>, configuration: Arc<Configuration>) -> Self {
        Self {
            schema,
            configuration,
        }
    }

    pub(crate) async fn supergraph_request(
        &self,
        request: PartialSupergraphRequest,
    ) -> Result<SupergraphRequest, SupergraphResponse> {
        let op_name = request.supergraph_request.body().operation_name.as_ref();
        let query = self
            .parse((
                request.supergraph_request.body().query.clone().unwrap(),
                op_name.cloned(),
            ))
            .await
            .expect("TODO[igni]");

        request
            .context
            .insert(
                "operation_name",
                query.operation(op_name).and_then(|op| op.name()),
            )
            .unwrap();

        Ok(SupergraphRequest {
            supergraph_request: request.supergraph_request,
            context: request.context,
            query: Some(query),
        })
    }

    pub(crate) async fn parse(&self, key: QueryKey) -> Result<Query, QueryPlannerError> {
        let (query, operation_name) = key;
        let schema = self.schema.clone();
        let configuration = self.configuration.clone();
        let task_result = tokio::task::spawn_blocking(move || {
            let mut query = Query::parse(query, &schema, &configuration)?;
            crate::spec::operation_limits::check(&configuration, &mut query, operation_name)?;
            Ok::<_, QueryPlannerError>(query)
        })
        .instrument(tracing::info_span!("parse_query", "otel.kind" = "INTERNAL"))
        .await;
        if let Err(err) = &task_result {
            failfast_debug!("parsing query task failed: {}", err);
        }
        task_result?
    }
}
