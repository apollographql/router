use std::fmt::Display;
use std::fmt::Formatter;
use std::hash::Hash;
use std::sync::Arc;

use apollo_compiler::ast;
use apollo_compiler::validation::DiagnosticList;
use apollo_compiler::ExecutableDocument;
use http::StatusCode;
use lru::LruCache;
use tokio::sync::Mutex;

use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::query_planner::OperationKind;
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
                    .cache
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

                let operation = doc.executable.get_operation(op_name.as_deref()).ok();
                let operation_name = operation
                    .as_ref()
                    .and_then(|operation| operation.name.as_ref().map(|s| s.as_str().to_owned()));

                context.insert(OPERATION_NAME, operation_name).unwrap();
                let operation_kind = operation.map(|op| OperationKind::from(op.operation_type));
                context
                    .insert(OPERATION_KIND, operation_kind.unwrap_or_default())
                    .expect("cannot insert operation kind in the context; this is a bug");

                if self.enable_authorization_directives {
                    AuthorizationPlugin::query_analysis(
                        &query,
                        &self.schema,
                        &self.configuration,
                        &context,
                    );
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
            .extensions()
            .lock()
            .insert::<ParsedDocument>(doc);

        Ok(SupergraphRequest {
            supergraph_request: request.supergraph_request,
            context: request.context,
        })
    }
}

pub(crate) type ParsedDocument = Arc<ParsedDocumentInner>;

#[derive(Debug, Default)]
pub(crate) struct ParsedDocumentInner {
    pub(crate) ast: ast::Document,
    pub(crate) executable: ExecutableDocument,
    pub(crate) parse_errors: Option<DiagnosticList>,
    pub(crate) validation_errors: Option<DiagnosticList>,
}

impl Display for ParsedDocumentInner {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Hash for ParsedDocumentInner {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.ast.hash(state);
    }
}

impl PartialEq for ParsedDocumentInner {
    fn eq(&self, other: &Self) -> bool {
        self.ast == other.ast
    }
}

impl Eq for ParsedDocumentInner {}
