use std::sync::Arc;

use http::StatusCode;

use crate::query_planner::QueryKey;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
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

    pub(crate) fn supergraph_request(
        &self,
        request: SupergraphRequest,
    ) -> Result<SupergraphRequest, SupergraphResponse> {
        let query = request.supergraph_request.body().query.as_ref();

        if query.is_none() || query.unwrap().trim().is_empty() {
            let errors = vec![crate::error::Error::builder()
                .message("Must provide query string.".to_string())
                .extension_code("MISSING_QUERY_STRING")
                .build()];
            tracing::error!(
                monotonic_counter.apollo_router_http_requests_total = 1u64,
                status = %StatusCode::BAD_REQUEST.as_u16(),
                error = "Must provide query string",
                "Must provide query string"
            );

            return Err(SupergraphResponse::builder()
                .errors(errors)
                .status_code(StatusCode::BAD_REQUEST)
                .context(request.context)
                .build()
                .expect("response is valid"));
        }

        let op_name = request.supergraph_request.body().operation_name.as_ref();
        let query = self.parse((
            request
                .supergraph_request
                .body()
                .query
                .clone()
                .expect("query presence was already checked"),
            op_name.cloned(),
        ));

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

    pub(crate) fn parse(&self, key: QueryKey) -> Query {
        let query = key.0;
        let schema = self.schema.clone();
        let configuration: Arc<Configuration> = self.configuration.clone();
        // TODO[igni]: profile and benchmark with and without the blocking task spawn
        tracing::info_span!("parse_query", "otel.kind" = "INTERNAL")
            .in_scope(|| Query::parse_unchecked(query, &schema, &configuration))
    }
}
