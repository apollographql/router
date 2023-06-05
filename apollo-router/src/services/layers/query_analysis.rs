use std::sync::Arc;

use apollo_compiler::ApolloCompiler;
use apollo_compiler::FileId;
use apollo_compiler::HirDatabase;
use http::StatusCode;
use lru::LruCache;
use tokio::sync::Mutex;

use crate::query_planner::QueryKey;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::spec::query::Operation;
use crate::spec::Query;
use crate::spec::Schema;
use crate::Configuration;

/// [`Layer`] for QueryParsing implementation.
#[derive(Clone)]
pub(crate) struct QueryAnalysisLayer {
    /// set to None if QueryParsing is disabled
    schema: Arc<Schema>,
    configuration: Arc<Configuration>,
    cache: Arc<Mutex<LruCache<QueryAnalysisKey, Arc<Mutex<ApolloCompiler>>>>>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct QueryAnalysisKey {
    query: String,
    operation_name: Option<String>,
}

impl QueryAnalysisLayer {
    pub(crate) async fn new(schema: Arc<Schema>, configuration: Arc<Configuration>) -> Self {
        let mut cache = configuration
            .supergraph
            .query_planning
            .experimental_cache
            .clone();
        cache.redis = None;

        Self {
            schema,
            configuration,
            cache: Arc::new(Mutex::new(LruCache::new(cache.in_memory.limit))),
        }
    }

    pub(crate) fn make_compiler(&self, query: &str) -> (ApolloCompiler, FileId) {
        let mut compiler = ApolloCompiler::new()
            .recursion_limit(
                self.configuration
                    .preview_operation_limits
                    .parser_max_recursion,
            )
            .token_limit(
                self.configuration
                    .preview_operation_limits
                    .parser_max_tokens,
            );
        compiler.set_type_system_hir(self.schema.type_system.clone());
        let id = compiler.add_executable(query, "query");
        (compiler, id)
    }

    pub(crate) async fn supergraph_request(
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

        let op_name = request.supergraph_request.body().operation_name.clone();
        let query = request
            .supergraph_request
            .body()
            .query
            .clone()
            .expect("query presence was already checked");
        let entry = self
            .cache
            .lock()
            .await
            .get(&QueryAnalysisKey {
                query: query.clone(),
                operation_name: op_name.clone(),
            })
            .cloned();

        let compiler = match entry {
            None => {
                let (compiler, file_id) = self.make_compiler(&query);

                let compiler = Arc::new(Mutex::new(compiler));

                (*self.cache.lock().await).put(
                    QueryAnalysisKey {
                        query,
                        operation_name: op_name.clone(),
                    },
                    compiler.clone(),
                );

                compiler
            }
            Some(c) => c,
        };

        let op = match op_name {
            None => compiler
                .lock()
                .await
                .db
                .all_operations()
                .iter()
                .filter_map(|operation| Operation::from_hir(operation, &self.schema).ok())
                .next(),
            Some(name) => compiler
                .lock()
                .await
                .db
                .all_operations()
                .iter()
                .filter_map(|operation| Operation::from_hir(operation, &self.schema).ok())
                .find(|op| {
                    if let Some(op_name) = op.name.as_deref() {
                        op_name == &name
                    } else {
                        false
                    }
                }),
        };

        request
            .context
            .insert("operation_name", op.and_then(|op| op.name()))
            .unwrap();

        Ok(SupergraphRequest {
            supergraph_request: request.supergraph_request,
            context: request.context,
            compiler: Some(compiler),
        })
    }

    pub(crate) fn parse(&self, key: QueryKey) -> Query {
        let query = key.0;
        let schema = self.schema.clone();
        let configuration: Arc<Configuration> = self.configuration.clone();
        tracing::info_span!("parse_query", "otel.kind" = "INTERNAL")
            .in_scope(|| Query::parse_unchecked(query, &schema, &configuration))
    }
}
