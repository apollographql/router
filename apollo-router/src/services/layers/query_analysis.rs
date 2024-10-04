use std::collections::HashMap;
use std::fmt::Display;
use std::fmt::Formatter;
use std::hash::Hash;
use std::sync::Arc;

use apollo_compiler::ast;
use apollo_compiler::executable::Operation;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Node;
use http::StatusCode;
use lru::LruCache;
use router_bridge::planner::UsageReporting;
use tokio::sync::Mutex;

use crate::apollo_studio_interop::generate_extended_references;
use crate::apollo_studio_interop::ExtendedReferenceStats;
use crate::compute_task;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::graphql::Error;
use crate::graphql::ErrorExtension;
use crate::graphql::IntoGraphQLErrors;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::telemetry::config::ApolloMetricsReferenceMode;
use crate::plugins::telemetry::config::Conf as TelemetryConfig;
use crate::plugins::telemetry::consts::QUERY_PARSING_SPAN_NAME;
use crate::query_planner::fetch::QueryHash;
use crate::query_planner::OperationKind;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::spec::Query;
use crate::spec::Schema;
use crate::spec::SpecError;
use crate::Configuration;
use crate::Context;

/// [`Layer`] for QueryAnalysis implementation.
#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub(crate) struct QueryAnalysisLayer {
    pub(crate) schema: Arc<Schema>,
    configuration: Arc<Configuration>,
    cache: Arc<Mutex<LruCache<QueryAnalysisKey, Result<(Context, ParsedDocument), SpecError>>>>,
    enable_authorization_directives: bool,
    metrics_reference_mode: ApolloMetricsReferenceMode,
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
        let metrics_reference_mode = TelemetryConfig::metrics_reference_mode(&configuration);

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
            metrics_reference_mode,
        }
    }

    pub(crate) async fn parse_document(
        &self,
        query: &str,
        operation_name: Option<&str>,
    ) -> Result<(ParsedDocument, Node<Operation>), SpecError> {
        let query = query.to_string();
        let operation_name = operation_name.map(|o| o.to_string());
        let schema = self.schema.clone();
        let conf = self.configuration.clone();

        // Must be created *outside* of the compute_task or the span is not connected to the
        // parent
        let span = tracing::info_span!(QUERY_PARSING_SPAN_NAME, "otel.kind" = "INTERNAL");

        compute_task::execute(move || {
            span.in_scope(|| {
                let doc = Query::parse_document(
                    &query,
                    operation_name.as_deref(),
                    schema.as_ref(),
                    conf.as_ref(),
                )?;
                let operation = doc.get_operation(operation_name.as_deref())?.clone();
                Ok((doc, operation))
            })
        })
        .await
        .expect("parse_document task panicked")
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

        let res = match entry {
            None => match self.parse_document(&query, op_name.as_deref()).await {
                Err(errors) => {
                    (*self.cache.lock().await).put(
                        QueryAnalysisKey {
                            query,
                            operation_name: op_name.clone(),
                        },
                        Err(errors.clone()),
                    );
                    Err(errors)
                }
                Ok((doc, operation)) => {
                    let context = Context::new();

                    if self.enable_authorization_directives {
                        AuthorizationPlugin::query_analysis(
                            &doc,
                            op_name.as_deref(),
                            &self.schema,
                            &context,
                        );
                    }

                    context
                        .insert(OPERATION_NAME, operation.name.clone())
                        .expect("cannot insert operation name into context; this is a bug");
                    let operation_kind = OperationKind::from(operation.operation_type);
                    context
                        .insert(OPERATION_KIND, operation_kind)
                        .expect("cannot insert operation kind in the context; this is a bug");

                    (*self.cache.lock().await).put(
                        QueryAnalysisKey {
                            query,
                            operation_name: op_name.clone(),
                        },
                        Ok((context.clone(), doc.clone())),
                    );

                    Ok((context, doc))
                }
            },
            Some(c) => c,
        };

        match res {
            Ok((context, doc)) => {
                request.context.extend(&context);

                let extended_ref_stats = if matches!(
                    self.metrics_reference_mode,
                    ApolloMetricsReferenceMode::Extended
                ) {
                    Some(generate_extended_references(
                        doc.executable.clone(),
                        op_name,
                        self.schema.api_schema(),
                        &request.supergraph_request.body().variables.clone(),
                    ))
                } else {
                    None
                };

                request.context.extensions().with_lock(|mut lock| {
                    lock.insert::<ParsedDocument>(doc.clone());
                    if let Some(stats) = extended_ref_stats {
                        lock.insert::<ExtendedReferenceStats>(stats);
                    }
                });

                Ok(SupergraphRequest {
                    supergraph_request: request.supergraph_request,
                    context: request.context,
                })
            }
            Err(errors) => {
                request.context.extensions().with_lock(|mut lock| {
                    lock.insert(Arc::new(UsageReporting {
                        stats_report_key: errors.get_error_key().to_string(),
                        referenced_fields_by_type: HashMap::new(),
                    }))
                });
                let errors = match errors.into_graphql_errors() {
                    Ok(v) => v,
                    Err(errors) => vec![Error::builder()
                        .message(errors.to_string())
                        .extension_code(errors.extension_code())
                        .build()],
                };
                Err(SupergraphResponse::builder()
                    .errors(errors)
                    .status_code(StatusCode::BAD_REQUEST)
                    .context(request.context)
                    .build()
                    .expect("response is valid"))
            }
        }
    }
}

pub(crate) type ParsedDocument = Arc<ParsedDocumentInner>;

#[derive(Debug)]
pub(crate) struct ParsedDocumentInner {
    pub(crate) ast: ast::Document,
    pub(crate) executable: Arc<Valid<ExecutableDocument>>,
    pub(crate) hash: Arc<QueryHash>,
}

impl ParsedDocumentInner {
    pub(crate) fn get_operation(
        &self,
        operation_name: Option<&str>,
    ) -> Result<&Node<Operation>, SpecError> {
        if let Ok(operation) = self.executable.operations.get(operation_name) {
            Ok(operation)
        } else if let Some(name) = operation_name {
            Err(SpecError::UnknownOperation(name.to_owned()))
        } else if self.executable.operations.is_empty() {
            // Maybe not reachable?
            // A valid document is non-empty and has no unused fragments
            Err(SpecError::NoOperation)
        } else {
            debug_assert!(self.executable.operations.len() > 1);
            Err(SpecError::MultipleOperationWithoutOperationName)
        }
    }
}

impl Display for ParsedDocumentInner {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Hash for ParsedDocumentInner {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.hash.0.hash(state);
    }
}

impl PartialEq for ParsedDocumentInner {
    fn eq(&self, other: &Self) -> bool {
        self.ast == other.ast
    }
}

impl Eq for ParsedDocumentInner {}
