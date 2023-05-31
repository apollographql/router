use std::sync::Arc;

use crate::query_planner::QueryKey;
use crate::services::SupergraphRequest;
use crate::spec::Query;
use crate::spec::Schema;
use crate::Configuration;

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

    pub(crate) fn supergraph_request(&self, request: SupergraphRequest) -> SupergraphRequest {
        let op_name = request.supergraph_request.body().operation_name.as_ref();
        let query = self.parse((
            request.supergraph_request.body().query.clone().unwrap(),
            op_name.cloned(),
        ));

        request
            .context
            .insert(
                "operation_name",
                query.operation(op_name).and_then(|op| op.name()),
            )
            .unwrap();

        SupergraphRequest {
            supergraph_request: request.supergraph_request,
            context: request.context,
            query: Some(query),
        }
    }

    pub(crate) fn parse(&self, key: QueryKey) -> Query {
        let query = key.0;
        let schema = self.schema.clone();
        let configuration: Arc<Configuration> = self.configuration.clone();
        // TODO[igni]: profile and benchmark with and without the blocking task spawn
        tracing::info_span!("parse_query", "otel.kind" = "INTERNAL")
            .in_scope(|| Query::parse_unchecked(query, &schema, &configuration))
    }
}
