use std::collections::HashMap;
use std::fmt::Display;
use std::fmt::Formatter;
use std::hash::Hash;
use std::sync::Arc;
use std::sync::OnceLock;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable::Operation;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::response::GraphQLError;
use apollo_compiler::validation::Valid;
use http::StatusCode;
use lru::LruCache;
use tokio::sync::Mutex;
use tracing::Instrument;

use crate::Configuration;
use crate::Context;
use crate::apollo_studio_interop::ExtendedReferenceStats;
use crate::apollo_studio_interop::UsageReporting;
use crate::apollo_studio_interop::generate_extended_references;
use crate::compute_job;
use crate::compute_job::ComputeJobType;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::error::ValidationErrors;
use crate::graphql::Error;
use crate::graphql::ErrorExtension;
use crate::graphql::IntoGraphQLErrors;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::telemetry::config::ApolloMetricsReferenceMode;
use crate::plugins::telemetry::config::Conf as TelemetryConfig;
use crate::plugins::telemetry::consts::QUERY_PARSING_SPAN_NAME;
use crate::query_planner::OperationKind;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::spec::Query;
use crate::spec::QueryHash;
use crate::spec::Schema;
use crate::spec::SpecError;

const ENV_DISABLE_RECURSIVE_SELECTIONS_CHECK: &str =
    "APOLLO_ROUTER_DISABLE_SECURITY_RECURSIVE_SELECTIONS_CHECK";
/// Should we enforce the recursive selections limit? Default true, can be toggled off with an
/// environment variable.
///
/// Disabling this check is very much not advisable and we don't expect that anyone will need to do
/// it. In the extremely unlikely case that the new protection breaks someone's legitimate queries,
/// though, they could temporarily disable this individual limit so they can still benefit from the
/// other new limits, until we improve the detection.
pub(crate) fn recursive_selections_check_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let disabled =
            std::env::var(ENV_DISABLE_RECURSIVE_SELECTIONS_CHECK).as_deref() == Ok("true");

        !disabled
    })
}

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
    const MAX_RECURSIVE_SELECTIONS: u32 = 10_000_000;

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
    ) -> Result<ParsedDocument, SpecError> {
        let query = query.to_string();
        let operation_name = operation_name.map(|o| o.to_string());
        let schema = self.schema.clone();
        let conf = self.configuration.clone();

        // Must be created *outside* of the spawn_blocking or the span is not connected to the
        // parent
        let span = tracing::info_span!(QUERY_PARSING_SPAN_NAME, "otel.kind" = "INTERNAL");
        let compute_job_future = span.in_scope(||{
            let job = move || {
                Query::parse_document(
                    &query,
                    operation_name.as_deref(),
                    schema.as_ref(),
                    conf.as_ref(),
                )
                    .and_then(|doc| {
                        let recursive_selections = Self::count_recursive_selections(
                            &doc.executable,
                            &mut Default::default(),
                            &doc.operation.selection_set,
                            0,
                        );
                        if recursive_selections.is_none() {
                            if recursive_selections_check_enabled() {
                                return Err(SpecError::ValidationError(ValidationErrors {
                                    errors: vec![GraphQLError {
                                        message:
                                        "Maximum recursive selections limit exceeded in this operation"
                                            .to_string(),
                                        locations: Default::default(),
                                        path: Default::default(),
                                        extensions: Default::default(),
                                    }],
                                }))
                            }
                            tracing::info!(
                            operation_name = ?operation_name,
                            limit = Self::MAX_RECURSIVE_SELECTIONS,
                            "operation exceeded maximum recursive selections limit, but limit is forcefully disabled",
                        );
                        }
                        Ok(doc)
                    })
            };
            let job = std::panic::AssertUnwindSafe(job);
            compute_job::execute(ComputeJobType::QueryParsing, job)
        });

        compute_job_future
            .instrument(span)
            .await
            .expect("Query::parse_document panicked")
    }

    /// Measure the number of selections that would be encountered if we walked the given selection
    /// set while recursing into fragment spreads, and add it to the given count. `None` is returned
    /// instead if this number exceeds `Self::MAX_RECURSIVE_SELECTIONS`.
    ///
    /// This function assumes that fragments referenced by spreads exist and that they don't form
    /// cycles. If a fragment spread appears multiple times for the same named fragment, it is
    /// counted multiple times.
    fn count_recursive_selections<'a>(
        document: &'a Valid<ExecutableDocument>,
        fragment_cache: &mut HashMap<&'a Name, u32>,
        selection_set: &'a SelectionSet,
        mut count: u32,
    ) -> Option<u32> {
        for selection in &selection_set.selections {
            count = count
                .checked_add(1)
                .take_if(|v| *v <= Self::MAX_RECURSIVE_SELECTIONS)?;
            match selection {
                Selection::Field(field) => {
                    count = Self::count_recursive_selections(
                        document,
                        fragment_cache,
                        &field.selection_set,
                        count,
                    )?;
                }
                Selection::InlineFragment(fragment) => {
                    count = Self::count_recursive_selections(
                        document,
                        fragment_cache,
                        &fragment.selection_set,
                        count,
                    )?;
                }
                Selection::FragmentSpread(fragment) => {
                    let name = &fragment.fragment_name;
                    if let Some(cached) = fragment_cache.get(name) {
                        count = count
                            .checked_add(*cached)
                            .take_if(|v| *v <= Self::MAX_RECURSIVE_SELECTIONS)?;
                    } else {
                        let old_count = count;
                        count = Self::count_recursive_selections(
                            document,
                            fragment_cache,
                            &document
                                .fragments
                                .get(&fragment.fragment_name)
                                .expect("validation should have ensured referenced fragments exist")
                                .selection_set,
                            count,
                        )?;
                        fragment_cache.insert(name, count - old_count);
                    };
                }
            }
        }
        Some(count)
    }

    pub(crate) async fn supergraph_request(
        &self,
        request: SupergraphRequest,
    ) -> Result<SupergraphRequest, SupergraphResponse> {
        let query = request.supergraph_request.body().query.as_ref();

        if query.is_none() || query.unwrap().trim().is_empty() {
            let errors = vec![
                crate::error::Error::builder()
                    .message("Must provide query string.".to_string())
                    .extension_code("MISSING_QUERY_STRING")
                    .build(),
            ];
            u64_counter!(
                "apollo_router_http_requests_total",
                "Total number of HTTP requests made. (deprecated)",
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
                Ok(doc) => {
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
                        .insert(OPERATION_NAME, doc.operation.name.clone())
                        .expect("cannot insert operation name into context; this is a bug");
                    let operation_kind = OperationKind::from(doc.operation.operation_type);
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
                        &request.supergraph_request.body().variables,
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
                    Err(errors) => vec![
                        Error::builder()
                            .message(errors.to_string())
                            .extension_code(errors.extension_code())
                            .build(),
                    ],
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
    pub(crate) operation: Node<Operation>,
    /// `__schema` or `__type`
    pub(crate) has_schema_introspection: bool,
    /// Non-meta fields explicitly defined in the schema
    pub(crate) has_explicit_root_fields: bool,
}

impl ParsedDocumentInner {
    pub(crate) fn new(
        ast: ast::Document,
        executable: Arc<Valid<ExecutableDocument>>,
        operation_name: Option<&str>,
        hash: Arc<QueryHash>,
    ) -> Result<Arc<Self>, SpecError> {
        let operation = get_operation(&executable, operation_name)?;
        let mut has_schema_introspection = false;
        let mut has_explicit_root_fields = false;
        for field in operation.root_fields(&executable) {
            match field.name.as_str() {
                "__typename" => {} // turns out we have no conditional on `has_root_typename`
                "__schema" | "__type" if operation.is_query() => has_schema_introspection = true,
                _ => has_explicit_root_fields = true,
            }
        }
        Ok(Arc::new(Self {
            ast,
            executable,
            hash,
            operation,
            has_schema_introspection,
            has_explicit_root_fields,
        }))
    }
}

pub(crate) fn get_operation(
    executable: &ExecutableDocument,
    operation_name: Option<&str>,
) -> Result<Node<Operation>, SpecError> {
    if let Ok(operation) = executable.operations.get(operation_name) {
        Ok(operation.clone())
    } else if let Some(name) = operation_name {
        Err(SpecError::UnknownOperation(name.to_owned()))
    } else if executable.operations.is_empty() {
        // Maybe not reachable?
        // A valid document is non-empty and has no unused fragments
        Err(SpecError::NoOperation)
    } else {
        debug_assert!(executable.operations.len() > 1);
        Err(SpecError::MultipleOperationWithoutOperationName)
    }
}

impl Display for ParsedDocumentInner {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl Hash for ParsedDocumentInner {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.hash.hash(state);
    }
}

impl PartialEq for ParsedDocumentInner {
    fn eq(&self, other: &Self) -> bool {
        self.ast == other.ast
    }
}

impl Eq for ParsedDocumentInner {}
