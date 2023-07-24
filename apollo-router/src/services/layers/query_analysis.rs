use std::sync::Arc;

use apollo_compiler::ApolloCompiler;
use apollo_compiler::FileId;
use apollo_compiler::HirDatabase;
use http::StatusCode;
use lru::LruCache;
use tokio::sync::Mutex;

use crate::context::OPERATION_NAME;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::spec::Query;
use crate::spec::Schema;
use crate::Configuration;
use crate::Context;

/// [`Layer`] for QueryAnalysis implementation.
#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub(crate) struct QueryAnalysisLayer {
    schema: Arc<Schema>,
    configuration: Arc<Configuration>,
    cache: Arc<Mutex<LruCache<QueryAnalysisKey, (Context, Arc<Mutex<ApolloCompiler>>)>>>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct QueryAnalysisKey {
    query: String,
    operation_name: Option<String>,
}

impl QueryAnalysisLayer {
    pub(crate) async fn new(schema: Arc<Schema>, configuration: Arc<Configuration>) -> Self {
        Self {
            schema,
            cache: Arc::new(Mutex::new(LruCache::new(
                configuration
                    .supergraph
                    .query_planning
                    .experimental_cache
                    .in_memory
                    .limit,
            ))),
            configuration,
        }
    }

    pub(crate) fn make_compiler(&self, query: &str) -> (ApolloCompiler, FileId) {
        Query::make_compiler(query, self.schema.api_schema(), &self.configuration)
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

        let (context, compiler) = match entry {
            None => {
                let span = tracing::info_span!("parse_query", "otel.kind" = "INTERNAL");
                let (compiler, file_id) = span.in_scope(|| self.make_compiler(&query));

                let compiler = Arc::new(Mutex::new(compiler));
                let context = Context::new();

                let operation_name = compiler
                    .lock()
                    .await
                    .db
                    .find_operation(file_id, op_name.clone())
                    .and_then(|operation| operation.name().map(|s| s.to_owned()));

                context.insert(OPERATION_NAME, operation_name).unwrap();

                (*self.cache.lock().await).put(
                    QueryAnalysisKey {
                        query,
                        operation_name: op_name,
                    },
                    (context.clone(), compiler.clone()),
                );

                (context, compiler)
            }
            Some(c) => c,
        };

        request.context.extend(&context);
        request
            .context
            .private_entries
            .lock()
            .insert(Compiler(compiler));

        Ok(SupergraphRequest {
            supergraph_request: request.supergraph_request,
            context: request.context,
        })
    }
}

pub(crate) struct Compiler(pub(crate) Arc<Mutex<ApolloCompiler>>);
