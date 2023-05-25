use std::collections::HashMap;
use std::sync::Arc;

use http::StatusCode;
use router_bridge::planner::UsageReporting;

use crate::error::QueryParserError;
use crate::graphql;
use crate::graphql::IntoGraphQLErrors;
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
        let query = match self
            .parse((
                request.supergraph_request.body().query.clone().unwrap(),
                op_name.cloned(),
            ))
            .await
        {
            Err(error) => {
                if let QueryParserError::SpecError(e) = &error {
                    request
                        .context
                        .private_entries
                        .lock()
                        .insert(UsageReporting {
                            stats_report_key: e.get_error_key().to_string(),
                            referenced_fields_by_type: HashMap::new(),
                        });
                }
                let gql_errors = error.into_graphql_errors();

                Err(SupergraphResponse::builder()
                    .context(request.context.clone())
                    .errors(gql_errors)
                    .status_code(StatusCode::BAD_REQUEST) // If it's a graphql error we return a status code 400
                    .build()
                    .expect("this response build must not fail"))
            }
            Ok(query) => Ok(query),
        }?;

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

    pub(crate) async fn parse(&self, key: QueryKey) -> Result<Query, QueryParserError> {
        let (query, operation_name) = key;
        let schema = self.schema.clone();
        let configuration = self.configuration.clone();
        // TODO[igni]: profile and benchmark with and without the blocking task spawn
        tracing::info_span!("parse_query", "otel.kind" = "INTERNAL").in_scope(|| {
            let mut query = Query::parse(query, &schema, &configuration)?;
            crate::spec::operation_limits::check(&configuration, &mut query, operation_name)?;
            Ok(query)
        })
    }
}
