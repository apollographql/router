use std::sync::Arc;

use apollo_compiler::ast;
use apollo_compiler::ExecutableDocument;
use http::StatusCode;
use lru::LruCache;
use tokio::sync::Mutex;

use crate::context::OPERATION_NAME;
use crate::plugins::authorization::AuthorizationPlugin;
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
    pub(crate) schema: Arc<Schema>,
    configuration: Arc<Configuration>,
    cache: Arc<Mutex<LruCache<QueryAnalysisKey, (Context, ParsedDocument)>>>,
    enable_authorization_directives: bool,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct QueryAnalysisKey {
    query: String,
    operation_name: Option<String>,
}

impl QueryAnalysisLayer {
    pub(crate) async fn new(schema: Arc<Schema>, configuration: Arc<Configuration>) -> Self {
        let enable_authorization_directives =
            AuthorizationPlugin::enable_directives(&configuration, &schema).unwrap_or(false);
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
            enable_authorization_directives,
            configuration,
        }
    }

    pub(crate) fn parse_document(&self, query: &str) -> ParsedDocument {
        Query::parse_document(query, self.schema.api_schema(), &self.configuration)
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
            u64_counter!(
                "apollo_router_http_requests_total",
                "Total number of HTTP requests made.",
                1,
                status = StatusCode::BAD_REQUEST.as_u16() as i64,
                error = "Must provide query string"
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

        let (context, doc) = match entry {
            None => {
                let span = tracing::info_span!("parse_query", "otel.kind" = "INTERNAL");
                let doc = span.in_scope(|| self.parse_document(&query));

                let context = Context::new();

                let operation_name = doc
                    .executable
                    .get_operation(op_name.as_deref())
                    .ok()
                    .and_then(|operation| operation.name().map(|s| s.as_str().to_owned()));

                context.insert(OPERATION_NAME, operation_name).unwrap();

                if self.enable_authorization_directives {
                    AuthorizationPlugin::query_analysis(
                        &query,
                        &self.schema,
                        &self.configuration,
                        &context,
                    )
                    .await;
                }

                (*self.cache.lock().await).put(
                    QueryAnalysisKey {
                        query,
                        operation_name: op_name,
                    },
                    (context.clone(), doc.clone()),
                );

                (context, doc)
            }
            Some(c) => c,
        };

        request.context.extend(&context);
        request
            .context
            .private_entries
            .lock()
            .insert::<ParsedDocument>(doc);

        Ok(SupergraphRequest {
            supergraph_request: request.supergraph_request,
            context: request.context,
        })
    }
}

pub(crate) type ParsedDocument = Arc<ParsedDocumentInner>;

pub(crate) struct ParsedDocumentInner {
    pub(crate) ast: ast::Document,
    pub(crate) executable: ExecutableDocument,
}
