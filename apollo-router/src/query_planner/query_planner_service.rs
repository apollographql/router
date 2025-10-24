//! Calls out to the apollo-federation crate

use std::fmt::Debug;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::OnceLock;
use std::task::Poll;
use std::time::Instant;

use apollo_compiler::Name;
use apollo_compiler::ast;
use apollo_federation::error::FederationError;
use apollo_federation::error::SingleFederationError;
use apollo_federation::query_plan::query_planner::QueryPlanOptions;
use apollo_federation::query_plan::query_planner::QueryPlanner;
use futures::future::BoxFuture;
use opentelemetry::KeyValue;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::metrics::ObservableGauge;
use parking_lot::Mutex;
use serde_json_bytes::Value;
use tower::Service;

use super::PlanNode;
use super::QueryKey;
use crate::Configuration;
use crate::apollo_studio_interop::generate_usage_reporting;
use crate::compute_job;
use crate::compute_job::ComputeJobType;
use crate::compute_job::MaybeBackPressureError;
use crate::error::FederationErrorBridge;
use crate::error::QueryPlannerError;
use crate::error::ServiceBuildError;
use crate::error::ValidationErrors;
use crate::graphql;
use crate::introspection::IntrospectionCache;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::metrics::meter_provider;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::plugins::authorization::UnauthorizedPaths;
use crate::plugins::telemetry::config::ApolloSignatureNormalizationAlgorithm;
use crate::plugins::telemetry::config::Conf as TelemetryConfig;
use crate::query_planner::convert::convert_root_query_plan_node;
use crate::query_planner::fetch::SubgraphSchema;
use crate::query_planner::fetch::SubgraphSchemas;
use crate::query_planner::labeler::add_defer_labels;
use crate::services::QueryPlannerContent;
use crate::services::QueryPlannerRequest;
use crate::services::QueryPlannerResponse;
use crate::services::layers::query_analysis::ParsedDocument;
use crate::services::layers::query_analysis::ParsedDocumentInner;
use crate::services::query_planner::PlanOptions;
use crate::spec::Query;
use crate::spec::Schema;
use crate::spec::SpecError;
use crate::spec::operation_limits::OperationLimits;

pub(crate) const RUST_QP_MODE: &str = "rust";
const UNSUPPORTED_FED1: &str = "fed1";
const INTERNAL_INIT_ERROR: &str = "internal";

const ENV_DISABLE_NON_LOCAL_SELECTIONS_CHECK: &str =
    "APOLLO_ROUTER_DISABLE_SECURITY_NON_LOCAL_SELECTIONS_CHECK";
/// Should we enforce the non-local selections limit? Default true, can be toggled off with an
/// environment variable.
///
/// Disabling this check is very much not advisable and we don't expect that anyone will need to do
/// it. In the extremely unlikely case that the new protection breaks someone's legitimate queries,
/// though, they could temporarily disable this individual limit so they can still benefit from the
/// other new limits, until we improve the detection.
pub(crate) fn non_local_selections_check_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let disabled =
            std::env::var(ENV_DISABLE_NON_LOCAL_SELECTIONS_CHECK).as_deref() == Ok("true");

        !disabled
    })
}

/// A query planner that calls out to the apollo-federation crate.
///
/// No caching is performed. To cache, wrap in a [`CachingQueryPlanner`].
#[derive(Clone)]
pub(crate) struct QueryPlannerService {
    planner: Arc<QueryPlanner>,
    schema: Arc<Schema>,
    subgraph_schemas: Arc<SubgraphSchemas>,
    configuration: Arc<Configuration>,
    enable_authorization_directives: bool,
    _federation_instrument: ObservableGauge<u64>,
    compute_jobs_queue_size_gauge: Arc<Mutex<Option<ObservableGauge<u64>>>>,
    signature_normalization_algorithm: ApolloSignatureNormalizationAlgorithm,
    introspection: Arc<IntrospectionCache>,
}

fn federation_version_instrument(federation_version: Option<i64>) -> ObservableGauge<u64> {
    meter_provider()
        .meter("apollo/router")
        .u64_observable_gauge("apollo.router.supergraph.federation")
        .with_callback(move |observer| {
            observer.observe(
                1,
                &[KeyValue::new(
                    "federation.version",
                    federation_version.unwrap_or(0),
                )],
            );
        })
        .init()
}

impl QueryPlannerService {
    fn create_planner(
        schema: &Schema,
        configuration: &Configuration,
    ) -> Result<Arc<QueryPlanner>, ServiceBuildError> {
        let config = configuration.rust_query_planner_config();
        let result = QueryPlanner::new(schema.federation_supergraph(), config);

        match &result {
            Err(FederationError::SingleFederationError(error)) => match error {
                SingleFederationError::UnsupportedFederationVersion { .. } => {
                    metric_rust_qp_init(Some(UNSUPPORTED_FED1));
                }
                SingleFederationError::UnsupportedFeature {
                    message: _,
                    kind: _,
                } => metric_rust_qp_init(Some(INTERNAL_INIT_ERROR)),
                _ => {
                    metric_rust_qp_init(Some(INTERNAL_INIT_ERROR));
                }
            },
            Err(_) => metric_rust_qp_init(Some(INTERNAL_INIT_ERROR)),
            Ok(_) => metric_rust_qp_init(None),
        }

        Ok(Arc::new(result.map_err(ServiceBuildError::QpInitError)?))
    }

    async fn plan_inner(
        &self,
        doc: &ParsedDocument,
        operation: Option<String>,
        plan_options: PlanOptions,
        compute_job_type: ComputeJobType,
        // Initialization code that needs mutable access to the plan,
        // before we potentially share it in Arc with a background thread
        // for "both" mode.
        init_query_plan_root_node: impl Fn(&mut PlanNode) -> Result<(), ValidationErrors>,
    ) -> Result<QueryPlanResult, MaybeBackPressureError<QueryPlannerError>> {
        let doc = doc.clone();
        let rust_planner = self.planner.clone();
        let job = move |status: compute_job::JobStatus<'_, _>| -> Result<_, QueryPlannerError> {
            let start = Instant::now();

            let check = move || status.check_for_cooperative_cancellation();
            let query_plan_options = QueryPlanOptions {
                override_conditions: plan_options.override_conditions,
                check_for_cooperative_cancellation: Some(&check),
                non_local_selections_limit_enabled: non_local_selections_check_enabled(),
                disabled_subgraph_names: Default::default(),
            };

            let result = operation
                .as_deref()
                .map(|n| Name::new(n).map_err(FederationError::from))
                .transpose()
                .and_then(|operation| {
                    rust_planner.build_query_plan(&doc.executable, operation, query_plan_options)
                });
            if let Err(FederationError::SingleFederationError(
                SingleFederationError::InternalUnmergeableFields { .. },
            )) = &result
            {
                u64_counter!(
                    "apollo.router.operations.query_planner.unmergeable_fields",
                    "Query planner caught attempting to merge unmergeable fields",
                    1
                );
            }
            let result = result.map_err(FederationErrorBridge::from);

            let elapsed = start.elapsed().as_secs_f64();
            match &result {
                Ok(_) => metric_query_planning_plan_duration(RUST_QP_MODE, elapsed, "success"),
                Err(FederationErrorBridge::Cancellation(e)) if e.contains("timeout") => {
                    metric_query_planning_plan_duration(RUST_QP_MODE, elapsed, "timeout")
                }
                Err(FederationErrorBridge::Cancellation(_)) => {
                    metric_query_planning_plan_duration(RUST_QP_MODE, elapsed, "cancelled")
                }
                Err(_) => metric_query_planning_plan_duration(RUST_QP_MODE, elapsed, "error"),
            }

            let plan = result?;
            let root_node = convert_root_query_plan_node(&plan);
            Ok((plan, root_node))
        };
        let (plan, mut root_node) = compute_job::execute(compute_job_type, job)
            .map_err(MaybeBackPressureError::TemporaryError)?
            .await?;
        if let Some(node) = &mut root_node {
            init_query_plan_root_node(node).map_err(QueryPlannerError::from)?;
        }

        Ok(QueryPlanResult {
            formatted_query_plan: Some(Arc::new(plan.to_string())),
            query_plan_root_node: root_node.map(Arc::new),
            evaluated_plan_count: plan.statistics.evaluated_plan_count.clone().into_inner() as u64,
            evaluated_plan_paths: plan.statistics.evaluated_plan_paths.clone().into_inner() as u64,
            path_weight_high_water_mark: plan.statistics.path_weight_high_water_mark,
        })
    }

    pub(crate) async fn new(
        schema: Arc<Schema>,
        configuration: Arc<Configuration>,
    ) -> Result<Self, ServiceBuildError> {
        let introspection = Arc::new(IntrospectionCache::new(&configuration));
        let planner = Self::create_planner(&schema, &configuration)?;

        let subgraph_schemas = Arc::new(
            planner
                .subgraph_schemas()
                .iter()
                .map(|(name, schema)| {
                    (
                        name.to_string(),
                        SubgraphSchema::new(schema.schema().clone()),
                    )
                })
                .collect(),
        );

        let enable_authorization_directives =
            AuthorizationPlugin::enable_directives(&configuration, &schema)?;
        let federation_instrument = federation_version_instrument(schema.federation_version());
        let signature_normalization_algorithm =
            TelemetryConfig::signature_normalization_algorithm(&configuration);

        Ok(Self {
            planner,
            schema,
            subgraph_schemas,
            enable_authorization_directives,
            configuration,
            _federation_instrument: federation_instrument,
            compute_jobs_queue_size_gauge: Default::default(),
            signature_normalization_algorithm,
            introspection,
        })
    }

    pub(crate) fn schema(&self) -> Arc<Schema> {
        self.schema.clone()
    }

    pub(crate) fn subgraph_schemas(&self) -> Arc<SubgraphSchemas> {
        self.subgraph_schemas.clone()
    }

    async fn parse_selections(
        &self,
        query: String,
        operation_name: Option<&str>,
        doc: &ParsedDocument,
        query_metrics_in: &mut OperationLimits<u32>,
    ) -> Result<Query, QueryPlannerError> {
        let executable = &doc.executable;
        crate::spec::operation_limits::check(
            query_metrics_in,
            &self.configuration,
            &query,
            executable,
            operation_name,
        )?;

        let (fragments, operation, defer_stats, schema_aware_hash) =
            Query::extract_query_information(&self.schema, &query, executable, operation_name)?;

        let subselections = crate::spec::query::subselections::collect_subselections(
            &self.configuration,
            &operation,
            &fragments.map,
            &defer_stats,
        )?;
        Ok(Query {
            string: query,
            fragments,
            operation,
            filtered_query: None,
            unauthorized: UnauthorizedPaths {
                paths: vec![],
                errors: AuthorizationPlugin::log_errors(&self.configuration),
            },
            subselections,
            defer_stats,
            is_original: true,
            schema_aware_hash,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn plan(
        &self,
        original_query: String,
        filtered_query: String,
        operation: Option<String>,
        key: CacheKeyMetadata,
        selections: Query,
        plan_options: PlanOptions,
        doc: &ParsedDocument,
        compute_job_type: ComputeJobType,
        query_metrics: OperationLimits<u32>,
    ) -> Result<QueryPlannerContent, MaybeBackPressureError<QueryPlannerError>> {
        let plan_result = self
            .plan_inner(
                doc,
                operation.clone(),
                plan_options,
                compute_job_type,
                |root_node| {
                    root_node.init_parsed_operations_and_hash_subqueries(&self.subgraph_schemas)?;
                    root_node.extract_authorization_metadata(self.schema.supergraph_schema(), &key);
                    Ok(())
                },
            )
            .await?;
        let QueryPlanResult {
            query_plan_root_node,
            formatted_query_plan,
            evaluated_plan_count,
            evaluated_plan_paths,
            path_weight_high_water_mark,
        } = plan_result;

        // If the query is filtered, we want to generate the signature using the original query and generate the
        // reference using the filtered query. To do this, we need to re-parse the original query here.
        let signature_doc = if original_query != filtered_query {
            Query::parse_document(
                &original_query,
                operation.clone().as_deref(),
                &self.schema,
                &self.configuration,
            )
            .unwrap_or(doc.clone())
        } else {
            doc.clone()
        };

        let usage_reporting = generate_usage_reporting(
            &signature_doc.executable,
            &doc.executable,
            &operation,
            self.schema.supergraph_schema(),
            &self.signature_normalization_algorithm,
        );

        if let Some(node) = query_plan_root_node {
            u64_histogram!(
                "apollo.router.query_planning.plan.evaluated_plans",
                "Number of query plans evaluated for a query before choosing the best one",
                evaluated_plan_count
            );
            u64_histogram!(
                "apollo.router.query_planning.plan.evaluated_paths",
                "Number of paths (including intermediate ones) considered to plan a query before starting to generate a plan",
                evaluated_plan_paths
            );
            u64_histogram!(
                "apollo.router.query_planning.plan.path_weight_high_water_mark",
                "High water mark of the number of in-memory paths (weighted by path size) during query planning",
                path_weight_high_water_mark
            );

            Ok(QueryPlannerContent::Plan {
                plan: Arc::new(super::QueryPlan {
                    usage_reporting: Arc::new(usage_reporting),
                    root: node,
                    formatted_query_plan,
                    query: Arc::new(selections),
                    query_metrics,
                    estimated_size: Default::default(),
                }),
            })
        } else {
            failfast_debug!("empty query plan");
            Err(QueryPlannerError::EmptyPlan(usage_reporting.get_stats_report_key()).into())
        }
    }
}

impl Service<QueryPlannerRequest> for QueryPlannerService {
    type Response = QueryPlannerResponse;

    type Error = MaybeBackPressureError<QueryPlannerError>;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: QueryPlannerRequest) -> Self::Future {
        let start = Instant::now();
        let QueryPlannerRequest {
            query: original_query,
            operation_name,
            document,
            metadata,
            plan_options,
            compute_job_type,
        } = req;

        let this = self.clone();
        let fut = async move {
            let mut doc = document;

            let api_schema = this.schema.api_schema();
            match add_defer_labels(api_schema, &doc.ast) {
                Err(e) => {
                    return Err(QueryPlannerError::SpecError(SpecError::TransformError(
                        e.to_string(),
                    ))
                    .into());
                }
                Ok(modified_query) => {
                    let executable_document = modified_query
                        .to_executable_validate(api_schema)
                        // Assume transformation creates a valid document: ignore conversion errors
                        .map_err(|e| {
                            QueryPlannerError::from(SpecError::ValidationError(e.into()))
                        })?;
                    let hash = this
                        .schema
                        .schema_id
                        .operation_hash(&modified_query.to_string(), operation_name.as_deref());
                    doc = ParsedDocumentInner::new(
                        modified_query,
                        Arc::new(executable_document),
                        operation_name.as_deref(),
                        Arc::new(hash),
                    )
                    .map_err(QueryPlannerError::from)?;
                }
            }

            let res = this
                .get(
                    QueryKey {
                        original_query,
                        filtered_query: doc.ast.to_string(),
                        operation_name: operation_name.to_owned(),
                        metadata,
                        plan_options,
                    },
                    doc,
                    compute_job_type,
                )
                .await;

            f64_histogram!(
                "apollo.router.query_planning.total.duration",
                "Duration of the time the router waited for a query plan, including both the queue time and planning time, in seconds.",
                start.elapsed().as_secs_f64()
            );

            match res {
                Ok(query_planner_content) => Ok(QueryPlannerResponse::builder()
                    .content(query_planner_content)
                    .build()),
                Err(e) => Err(e),
            }
        };

        // Return the response as an immediate future
        Box::pin(fut)
    }
}

// Appease clippy::type_complexity
pub(crate) type FilteredQuery = (Vec<Path>, ast::Document);

impl QueryPlannerService {
    async fn get(
        &self,
        mut key: QueryKey,
        mut doc: ParsedDocument,
        compute_job_type: ComputeJobType,
    ) -> Result<QueryPlannerContent, MaybeBackPressureError<QueryPlannerError>> {
        let mut query_metrics = Default::default();
        let mut selections = self
            .parse_selections(
                key.original_query.clone(),
                key.operation_name.as_deref(),
                &doc,
                &mut query_metrics,
            )
            .await?;

        if selections.operation.selection_set.is_empty() {
            // All selections have @skip(true) or @include(false)
            // Return an empty response now to avoid dealing with an empty query plan later
            return Ok(QueryPlannerContent::Response {
                response: Box::new(
                    graphql::Response::builder()
                        .data(Value::Object(Default::default()))
                        .build(),
                ),
            });
        }

        match self
            .introspection
            .maybe_execute(&self.schema, &key, &doc)
            .await
        {
            ControlFlow::Continue(()) => (),
            ControlFlow::Break(result) => {
                return Ok(QueryPlannerContent::CachedIntrospectionResponse {
                    response: Box::new(result.map_err(MaybeBackPressureError::TemporaryError)?),
                });
            }
        }

        let filter_res = if self.enable_authorization_directives {
            match AuthorizationPlugin::filter_query(&self.configuration, &key, &self.schema) {
                Err(QueryPlannerError::Unauthorized(unauthorized_paths)) => {
                    let response = graphql::Response::builder()
                        .data(Object::new())
                        .errors(
                            unauthorized_paths
                                .into_iter()
                                .map(|path| {
                                    graphql::Error::builder()
                                        .message("Unauthorized field or type")
                                        .path(path)
                                        .extension_code("UNAUTHORIZED_FIELD_OR_TYPE")
                                        .build()
                                })
                                .collect(),
                        )
                        .build();
                    return Ok(QueryPlannerContent::Response {
                        response: Box::new(response),
                    });
                }
                other => other?,
            }
        } else {
            None
        };

        if let Some((unauthorized_paths, new_doc)) = filter_res {
            let new_query = new_doc.to_string();
            let new_hash = self
                .schema
                .schema_id
                .operation_hash(&new_query, key.operation_name.as_deref());

            key.filtered_query = new_query;
            let executable_document = new_doc
                .to_executable_validate(self.schema.api_schema())
                .map_err(|e| QueryPlannerError::from(SpecError::ValidationError(e.into())))?;
            doc = ParsedDocumentInner::new(
                new_doc,
                Arc::new(executable_document),
                key.operation_name.as_deref(),
                Arc::new(new_hash),
            )
            .map_err(QueryPlannerError::from)?;
            selections.unauthorized.paths = unauthorized_paths;
        }

        if key.filtered_query != key.original_query {
            let mut filtered = self
                .parse_selections(
                    key.filtered_query.clone(),
                    key.operation_name.as_deref(),
                    &doc,
                    &mut query_metrics,
                )
                .await?;
            filtered.is_original = false;
            selections.filtered_query = Some(Arc::new(filtered));
        }

        self.plan(
            key.original_query,
            key.filtered_query,
            key.operation_name,
            key.metadata,
            selections,
            key.plan_options,
            &doc,
            compute_job_type,
            query_metrics,
        )
        .await
    }

    pub(super) fn activate(&self) {
        // Gauges MUST be initialized after a meter provider is created.
        // When a hot reload happens this means that the gauges must be re-initialized.
        *self.compute_jobs_queue_size_gauge.lock() =
            Some(crate::compute_job::create_queue_size_gauge());
        self.introspection.activate();
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct QueryPlanResult {
    pub(super) formatted_query_plan: Option<Arc<String>>,
    pub(super) query_plan_root_node: Option<Arc<PlanNode>>,
    pub(super) evaluated_plan_count: u64,
    pub(super) evaluated_plan_paths: u64,
    pub(super) path_weight_high_water_mark: u64,
}

pub(crate) fn metric_query_planning_plan_duration(
    planner: &'static str,
    elapsed: f64,
    outcome: &'static str,
) {
    f64_histogram!(
        "apollo.router.query_planning.plan.duration",
        "Duration of the query planning, in seconds.",
        elapsed,
        "planner" = planner,
        "outcome" = outcome
    );
}

pub(crate) fn metric_rust_qp_init(init_error_kind: Option<&'static str>) {
    if let Some(init_error_kind) = init_error_kind {
        u64_counter!(
            "apollo.router.lifecycle.query_planner.init",
            "Rust query planner initialization",
            1,
            "init.error_kind" = init_error_kind,
            "init.is_success" = false
        );
    } else {
        u64_counter!(
            "apollo.router.lifecycle.query_planner.init",
            "Rust query planner initialization",
            1,
            "init.is_success" = true
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use test_log::test;
    use tower::ServiceExt;

    use super::*;
    use crate::metrics::FutureMetricsExt as _;
    use crate::services::subgraph;
    use crate::services::supergraph;
    use crate::spec::query::subselections::SubSelectionKey;
    use crate::spec::query::subselections::SubSelectionValue;

    const EXAMPLE_SCHEMA: &str = include_str!("testdata/schema.graphql");

    #[test(tokio::test)]
    async fn test_plan() {
        let result = plan(
            EXAMPLE_SCHEMA,
            include_str!("testdata/query.graphql"),
            include_str!("testdata/query.graphql"),
            None,
            PlanOptions::default(),
        )
        .await
        .unwrap();

        if let QueryPlannerContent::Plan { plan, .. } = result {
            insta::with_settings!({sort_maps => true}, {
                insta::assert_json_snapshot!("plan_usage_reporting", plan.usage_reporting);
            });
            insta::assert_debug_snapshot!("plan_root", plan.root);
        } else {
            panic!()
        }
    }

    #[test(tokio::test)]
    async fn federation_versions() {
        let sdl = include_str!("../testdata/minimal_fed1_supergraph.graphql");
        let config = Arc::default();
        let schema = Schema::parse(sdl, &config).unwrap();
        let error = QueryPlannerService::new(schema.into(), config)
            .await
            .err()
            .expect("expected error for fed1 supergraph");
        assert_eq!(
            error.to_string(),
            "failed to initialize the query planner: Supergraphs composed with federation version 1 are not supported. Please recompose your supergraph with federation version 2 or greater"
        );

        async {
            let sdl = include_str!("../testdata/minimal_supergraph.graphql");
            let config = Arc::default();
            let schema = Schema::parse(sdl, &config).unwrap();
            let _planner = QueryPlannerService::new(schema.into(), config)
                .await
                .unwrap();

            assert_gauge!(
                "apollo.router.supergraph.federation",
                1,
                federation.version = 2
            );
        }
        .with_metrics()
        .await;
    }

    #[test(tokio::test)]
    async fn empty_query_plan_should_be_a_planner_error() {
        let config = Default::default();
        let schema = Arc::new(Schema::parse(EXAMPLE_SCHEMA, &config).unwrap());
        let query = include_str!("testdata/unknown_introspection_query.graphql");

        let planner = QueryPlannerService::new(schema.clone(), Default::default())
            .await
            .unwrap();

        let doc = Query::parse_document(query, None, &schema, &Configuration::default()).unwrap();

        let mut query_metrics = Default::default();
        let selections = planner
            .parse_selections(query.to_string(), None, &doc, &mut query_metrics)
            .await
            .unwrap();
        let err =
            // test the planning part separately because it is a valid introspection query
            // it should be caught by the introspection part, but just in case, we check
            // that the query planner would return an empty plan error if it received an
            // introspection query
            planner.plan(
                include_str!("testdata/unknown_introspection_query.graphql").to_string(),
                include_str!("testdata/unknown_introspection_query.graphql").to_string(),
                None,
                CacheKeyMetadata::default(),
                selections,
                PlanOptions::default(),
                &doc,
                ComputeJobType::QueryPlanning,
                query_metrics
            )
                .await
                .unwrap_err();

        match err {
            MaybeBackPressureError::PermanentError(QueryPlannerError::EmptyPlan(
                stats_report_key,
            )) => {
                insta::with_settings!({sort_maps => true}, {
                    insta::assert_json_snapshot!("empty_query_plan_usage_reporting", stats_report_key);
                });
            }
            e => {
                panic!("empty plan should have returned an EmptyPlanError: {e:?}");
            }
        }
    }

    #[test(tokio::test)]
    async fn test_plan_error() {
        let query = "";
        let result = plan(EXAMPLE_SCHEMA, query, query, None, PlanOptions::default()).await;

        assert_eq!(
            "spec error: parsing error: syntax error: Unexpected <EOF>.",
            result.unwrap_err().to_string()
        );
    }

    #[test(tokio::test)]
    async fn test_single_aliased_root_typename() {
        let result = plan(
            EXAMPLE_SCHEMA,
            "{ x: __typename }",
            "{ x: __typename }",
            None,
            PlanOptions::default(),
        )
        .await
        .unwrap();
        if let QueryPlannerContent::CachedIntrospectionResponse { response } = result {
            assert_eq!(
                r#"{"data":{"x":"Query"}}"#,
                serde_json::to_string(&response).unwrap()
            )
        } else {
            panic!();
        }
    }

    #[test(tokio::test)]
    async fn test_two_root_typenames() {
        let result = plan(
            EXAMPLE_SCHEMA,
            "{ x: __typename __typename }",
            "{ x: __typename __typename }",
            None,
            PlanOptions::default(),
        )
        .await
        .unwrap();
        if let QueryPlannerContent::CachedIntrospectionResponse { response } = result {
            assert_eq!(
                r#"{"data":{"x":"Query","__typename":"Query"}}"#,
                serde_json::to_string(&response).unwrap()
            )
        } else {
            panic!();
        }
    }

    #[test(tokio::test)]
    async fn test_subselections() {
        let mut configuration: Configuration = Default::default();
        configuration.supergraph.introspection = true;
        let configuration = Arc::new(configuration);

        let schema = Schema::parse(EXAMPLE_SCHEMA, &configuration).unwrap();
        let planner = QueryPlannerService::new(schema.into(), configuration.clone())
            .await
            .unwrap();

        macro_rules! s {
            ($query: expr) => {
                insta::assert_snapshot!(subselections_keys($query, &planner).await);
            };
        }
        s!("query Q { me { username name { first last }}}");
        s!(r#"query Q { me {
            username
            name {
                first
                ... @defer(label: "A") { last }
            }
        }}"#);
        s!(r#"query Q { me {
            username
            name {
                ... @defer(label: "A") { first }
                ... @defer(label: "B") { last }
            }
        }}"#);
        // Aliases
        // FIXME: uncomment myName alias when this is fixed:
        // https://github.com/apollographql/router/issues/3263
        s!(r#"query Q { me {
            username
            # myName:
             name {
                firstName: first
                ... @defer(label: "A") { lastName: last }
            }
        }}"#);
        // Arguments
        s!(r#"query Q { user(id: 42) {
            username
            name {
                first
                ... @defer(label: "A") { last }
            }
        }}"#);
        // Type condition
        s!(r#"query Q { me {
            username
            ... on User {
                name {
                    first
                    ... @defer(label: "A") { last }
                }
            }
        }}"#);
        s!(r#"query Q { me {
            username
            ... on User @defer(label: "A") {
                name { first last }
            }
        }}"#);
        s!(r#"query Q { me {
            username
            ... @defer(label: "A") {
                ... on User {
                    name { first last }
                }
            }
        }}"#);
        // Array + argument
        s!(r#"query Q { me {
            id
            reviews {
                id
                ... @defer(label: "A") { body(format: true) }
            }
        }}"#);
        // Fragment spread becomes inline fragment
        s!(r#"
            query Q { me { username name { ... FirstLast @defer(label: "A") }}}
            fragment FirstLast on Name { first last }
        "#);
        s!(r#"
            query Q { me { username name { ... FirstLast @defer(label: "A") }}}
            fragment FirstLast on Name { first ... @defer(label: "B") { last }}
        "#);
        // Nested
        s!(r#"query Q { me {
            username
            ... @defer(label: "A") { name {
                first
                ... @defer(label: "B") { last }
            }}
        }}"#);
        s!(r#"query Q { me {
            id
            ... @defer(label: "A") {
                username
                ... @defer(label: "B") { name {
                    first
                    ... @defer(label: "C") { last }
                }}
            }
        }}"#);
        // Conditional
        s!(r#"query Q($d1:Boolean!) { me {
            username
            name {
                first
                ... @defer(if: $d1, label: "A") { last }
            }
        }}"#);
        s!(r#"query Q($d1:Boolean!, $d2:Boolean!) { me {
            username
            ... @defer(if: $d1, label: "A") { name {
                first
                ... @defer(if: $d2, label: "B") { last }
            }}
        }}"#);
        s!(r#"query Q($d1:Boolean!, $d2:Boolean!) { me {
            username
            name {
                ... @defer(if: $d1, label: "A") { first }
                ... @defer(if: $d2, label: "B") { last }
            }
        }}"#);
        // Mixed conditional and unconditional
        s!(r#"query Q($d1:Boolean!) { me {
            username
            name {
                ... @defer(label: "A") { first }
                ... @defer(if: $d1, label: "B") { last }
            }
        }}"#);
        // Include/skip
        s!(r#"query Q($s1:Boolean!) { me {
            username
            name {
                ... @defer(label: "A") {
                    first
                    last @skip(if: $s1)
                }
            }
        }}"#);
    }

    async fn subselections_keys(query: &str, planner: &QueryPlannerService) -> String {
        fn check_query_plan_coverage(
            node: &PlanNode,
            parent_label: Option<&str>,
            subselections: &HashMap<SubSelectionKey, SubSelectionValue>,
        ) {
            match node {
                PlanNode::Defer { primary, deferred } => {
                    if let Some(subselection) = primary.subselection.clone() {
                        assert!(
                            subselections
                                .keys()
                                .any(|k| k.defer_label.as_deref() == parent_label),
                            "Missing key: '{:?}' '{}' in {:?}",
                            parent_label,
                            subselection,
                            subselections.keys().collect::<Vec<_>>()
                        );
                    }
                    for deferred in deferred {
                        if let Some(subselection) = deferred.subselection.clone() {
                            let path = deferred.query_path.clone();
                            assert!(
                                subselections
                                    .keys()
                                    .any(|k| k.defer_label.as_deref() == deferred.label.as_deref()),
                                "Missing key: '{}' '{:?}' '{}'",
                                path,
                                deferred.label,
                                subselection
                            );
                        }
                        if let Some(node) = &deferred.node {
                            check_query_plan_coverage(
                                node,
                                deferred.label.as_deref(),
                                subselections,
                            )
                        }
                    }
                }
                PlanNode::Sequence { nodes } | PlanNode::Parallel { nodes } => {
                    for node in nodes {
                        check_query_plan_coverage(node, parent_label, subselections)
                    }
                }
                PlanNode::Fetch(_) => {}
                PlanNode::Flatten(flatten) => {
                    check_query_plan_coverage(&flatten.node, parent_label, subselections)
                }
                PlanNode::Condition {
                    condition: _,
                    if_clause,
                    else_clause,
                } => {
                    if let Some(node) = if_clause {
                        check_query_plan_coverage(node, parent_label, subselections)
                    }
                    if let Some(node) = else_clause {
                        check_query_plan_coverage(node, parent_label, subselections)
                    }
                }
                PlanNode::Subscription { rest, .. } => {
                    if let Some(node) = rest {
                        check_query_plan_coverage(node, parent_label, subselections)
                    }
                }
            }
        }

        fn serialize_selection_set(selection_set: &[crate::spec::Selection], to: &mut String) {
            if let Some((first, rest)) = selection_set.split_first() {
                to.push_str("{ ");
                serialize_selection(first, to);
                for sel in rest {
                    to.push(' ');
                    serialize_selection(sel, to);
                }
                to.push_str(" }");
            }
        }

        fn serialize_selection(selection: &crate::spec::Selection, to: &mut String) {
            match selection {
                crate::spec::Selection::Field {
                    name,
                    alias,
                    selection_set,
                    ..
                } => {
                    if let Some(alias) = alias {
                        to.push_str(alias.as_str());
                        to.push_str(": ");
                    }
                    to.push_str(name.as_str());
                    if let Some(sel) = selection_set {
                        to.push(' ');
                        serialize_selection_set(sel, to)
                    }
                }
                crate::spec::Selection::InlineFragment {
                    type_condition,
                    selection_set,
                    ..
                } => {
                    to.push_str("... on ");
                    to.push_str(type_condition);
                    serialize_selection_set(selection_set, to)
                }
                crate::spec::Selection::FragmentSpread { .. } => unreachable!(),
            }
        }

        let mut configuration: Configuration = Default::default();
        configuration.supergraph.introspection = true;
        let configuration = Arc::new(configuration);

        let doc = Query::parse_document(query, None, &planner.schema(), &configuration).unwrap();

        let result = planner
            .get(
                QueryKey {
                    original_query: query.to_string(),
                    filtered_query: query.to_string(),
                    operation_name: None,
                    metadata: CacheKeyMetadata::default(),
                    plan_options: PlanOptions::default(),
                },
                doc,
                ComputeJobType::QueryPlanning,
            )
            .await
            .unwrap();

        if let QueryPlannerContent::Plan { plan, .. } = result {
            check_query_plan_coverage(&plan.root, None, &plan.query.subselections);

            let mut keys: Vec<String> = Vec::new();
            for (key, value) in plan.query.subselections.iter() {
                let mut serialized = String::from("query");
                serialize_selection_set(&value.selection_set, &mut serialized);
                keys.push(format!(
                    "{:?} {} {}",
                    key.defer_label, key.defer_conditions.bits, serialized
                ))
            }
            keys.sort();
            keys.join("\n")
        } else {
            panic!()
        }
    }

    async fn plan(
        schema: &str,
        original_query: &str,
        filtered_query: &str,
        operation_name: Option<String>,
        plan_options: PlanOptions,
    ) -> Result<QueryPlannerContent, QueryPlannerError> {
        let mut configuration: Configuration = Default::default();
        configuration.supergraph.introspection = true;
        let configuration = Arc::new(configuration);

        let schema = Schema::parse(schema, &configuration).unwrap();
        let planner = QueryPlannerService::new(schema.into(), configuration.clone())
            .await
            .unwrap();

        let doc = Query::parse_document(
            original_query,
            operation_name.as_deref(),
            &planner.schema(),
            &configuration,
        )?;

        let result = planner
            .get(
                QueryKey {
                    original_query: original_query.to_string(),
                    filtered_query: filtered_query.to_string(),
                    operation_name,
                    metadata: CacheKeyMetadata::default(),
                    plan_options,
                },
                doc,
                ComputeJobType::QueryPlanning,
            )
            .await;
        match result {
            Ok(x) => Ok(x),
            Err(MaybeBackPressureError::PermanentError(e)) => Err(e),
            Err(MaybeBackPressureError::TemporaryError(e)) => panic!("{e:?}"),
        }
    }

    #[tokio::test]
    async fn test_rust_mode_subgraph_operation_serialization() {
        let subgraph_queries = Arc::new(tokio::sync::Mutex::new(String::new()));
        let subgraph_queries2 = Arc::clone(&subgraph_queries);
        let mut harness = crate::TestHarness::builder()
            // auth is not relevant here, but supergraph.graphql uses join/v0.1
            // which is not supported by the Rust query planner
            .schema(include_str!("../../tests/fixtures/supergraph-auth.graphql"))
            .subgraph_hook(move |_name, _default| {
                let subgraph_queries = Arc::clone(&subgraph_queries);
                tower::service_fn(move |request: subgraph::Request| {
                    let subgraph_queries = Arc::clone(&subgraph_queries);
                    async move {
                        let query = request
                            .subgraph_request
                            .body()
                            .query
                            .as_deref()
                            .unwrap_or_default();
                        let mut queries = subgraph_queries.lock().await;
                        queries.push_str(query);
                        queries.push('\n');
                        Ok(subgraph::Response::builder()
                            .extensions(crate::json_ext::Object::new())
                            .context(request.context)
                            .subgraph_name(String::default())
                            .build())
                    }
                })
                .boxed()
            })
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query("{ topProducts { name }}")
            .build()
            .unwrap();
        let mut response = harness.ready().await.unwrap().call(request).await.unwrap();
        assert!(response.response.status().is_success());
        let response = response.next_response().await.unwrap();
        assert!(response.errors.is_empty());

        let subgraph_queries = subgraph_queries2.lock().await;
        insta::assert_snapshot!(*subgraph_queries, @r###"
        { topProducts { name } }
        "###)
    }

    #[test]
    fn test_metric_query_planning_plan_duration() {
        let start = Instant::now();
        let elapsed = start.elapsed().as_secs_f64();
        metric_query_planning_plan_duration(RUST_QP_MODE, elapsed, "success");
        assert_histogram_exists!(
            "apollo.router.query_planning.plan.duration",
            f64,
            "planner" = "rust",
            "outcome" = "success"
        );
    }

    #[test]
    fn test_metric_rust_qp_initialization() {
        metric_rust_qp_init(None);
        assert_counter!(
            "apollo.router.lifecycle.query_planner.init",
            1,
            "init.is_success" = true
        );
        metric_rust_qp_init(Some(UNSUPPORTED_FED1));
        assert_counter!(
            "apollo.router.lifecycle.query_planner.init",
            1,
            "init.error_kind" = "fed1",
            "init.is_success" = false
        );
        metric_rust_qp_init(Some(INTERNAL_INIT_ERROR));
        assert_counter!(
            "apollo.router.lifecycle.query_planner.init",
            1,
            "init.error_kind" = "internal",
            "init.is_success" = false
        );
    }

    #[test(tokio::test)]
    async fn test_evaluated_plans_histogram() {
        async {
            let _ = plan(
                EXAMPLE_SCHEMA,
                include_str!("testdata/query.graphql"),
                include_str!("testdata/query.graphql"),
                None,
                PlanOptions::default(),
            )
            .await
            .unwrap();

            assert_histogram_exists!("apollo.router.query_planning.plan.evaluated_plans", u64);
        }
        .with_metrics()
        .await;
    }
}
