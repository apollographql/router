//! Calls out to nodejs query planner

use std::collections::HashMap;
use std::fmt::Debug;
use std::fmt::Write;
use std::sync::Arc;
use std::time::Instant;

use apollo_compiler::ast;
use apollo_compiler::execution::InputCoercionError;
use apollo_compiler::validation::Valid;
use apollo_compiler::Name;
use apollo_federation::error::FederationError;
use apollo_federation::error::SingleFederationError;
use apollo_federation::query_plan::query_planner::QueryPlanner;
use futures::future::BoxFuture;
use opentelemetry_api::metrics::MeterProvider as _;
use opentelemetry_api::metrics::ObservableGauge;
use opentelemetry_api::KeyValue;
use router_bridge::introspect::IntrospectionError;
use router_bridge::planner::PlanOptions;
use router_bridge::planner::PlanSuccess;
use router_bridge::planner::Planner;
use router_bridge::planner::UsageReporting;
use serde::Deserialize;
use serde_json_bytes::Value;
use tower::Service;

use super::PlanNode;
use super::QueryKey;
use crate::apollo_studio_interop::generate_usage_reporting;
use crate::cache::storage::CacheStorage;
use crate::configuration::IntrospectionMode as IntrospectionConfig;
use crate::configuration::QueryPlannerMode;
use crate::error::PlanErrors;
use crate::error::QueryPlannerError;
use crate::error::SchemaError;
use crate::error::ServiceBuildError;
use crate::error::ValidationErrors;
use crate::graphql;
use crate::graphql::Response;
use crate::introspection::Introspection;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::metrics::meter_provider;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::plugins::authorization::UnauthorizedPaths;
use crate::plugins::progressive_override::LABELS_TO_OVERRIDE_KEY;
use crate::plugins::telemetry::config::ApolloSignatureNormalizationAlgorithm;
use crate::plugins::telemetry::config::Conf as TelemetryConfig;
use crate::query_planner::convert::convert_root_query_plan_node;
use crate::query_planner::dual_query_planner::BothModeComparisonJob;
use crate::query_planner::fetch::QueryHash;
use crate::query_planner::labeler::add_defer_labels;
use crate::services::layers::query_analysis::ParsedDocument;
use crate::services::layers::query_analysis::ParsedDocumentInner;
use crate::services::QueryPlannerContent;
use crate::services::QueryPlannerRequest;
use crate::services::QueryPlannerResponse;
use crate::spec::operation_limits::OperationLimits;
use crate::spec::query::change::QueryHashVisitor;
use crate::spec::Query;
use crate::spec::Schema;
use crate::spec::SpecError;
use crate::Configuration;

pub(crate) const RUST_QP_MODE: &str = "rust";
pub(crate) const JS_QP_MODE: &str = "js";
const UNSUPPORTED_CONTEXT: &str = "context";
const UNSUPPORTED_OVERRIDES: &str = "overrides";
const UNSUPPORTED_FED1: &str = "fed1";
const INTERNAL_INIT_ERROR: &str = "internal";

#[derive(Clone)]
/// A query planner that calls out to the nodejs router-bridge query planner.
///
/// No caching is performed. To cache, wrap in a [`CachingQueryPlanner`].
pub(crate) struct BridgeQueryPlanner {
    planner: PlannerMode,
    schema: Arc<Schema>,
    subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
    introspection: IntrospectionMode,
    configuration: Arc<Configuration>,
    enable_authorization_directives: bool,
    _federation_instrument: ObservableGauge<u64>,
    signature_normalization_algorithm: ApolloSignatureNormalizationAlgorithm,
}

#[derive(Clone)]
pub(crate) enum PlannerMode {
    Js(Arc<Planner<QueryPlanResult>>),
    Both {
        js: Arc<Planner<QueryPlanResult>>,
        rust: Arc<QueryPlanner>,
    },
    Rust(Arc<QueryPlanner>),
}

#[derive(Clone)]
enum IntrospectionMode {
    Js(Arc<Introspection>),
    Both(Arc<Introspection>),
    Rust,
    Disabled,
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

impl PlannerMode {
    async fn new(
        schema: &Schema,
        configuration: &Configuration,
        old_planner: &Option<Arc<Planner<QueryPlanResult>>>,
        rust_planner: Option<Arc<QueryPlanner>>,
    ) -> Result<Self, ServiceBuildError> {
        Ok(match configuration.experimental_query_planner_mode {
            QueryPlannerMode::New => Self::Rust(
                rust_planner
                    .expect("expected Rust QP instance for `experimental_query_planner_mode: new`"),
            ),
            QueryPlannerMode::Legacy => {
                Self::Js(Self::js_planner(&schema.raw_sdl, configuration, old_planner).await?)
            }
            QueryPlannerMode::Both => Self::Both {
                js: Self::js_planner(&schema.raw_sdl, configuration, old_planner).await?,
                rust: rust_planner.expect(
                    "expected Rust QP instance for `experimental_query_planner_mode: both`",
                ),
            },
            QueryPlannerMode::BothBestEffort => {
                if let Some(rust) = rust_planner {
                    Self::Both {
                        js: Self::js_planner(&schema.raw_sdl, configuration, old_planner).await?,
                        rust,
                    }
                } else {
                    Self::Js(Self::js_planner(&schema.raw_sdl, configuration, old_planner).await?)
                }
            }
        })
    }

    pub(crate) fn maybe_rust(
        schema: &Schema,
        configuration: &Configuration,
    ) -> Result<Option<Arc<QueryPlanner>>, ServiceBuildError> {
        match configuration.experimental_query_planner_mode {
            QueryPlannerMode::Legacy => Ok(None),
            QueryPlannerMode::New | QueryPlannerMode::Both => {
                Ok(Some(Self::rust(schema, configuration)?))
            }
            QueryPlannerMode::BothBestEffort => match Self::rust(schema, configuration) {
                Ok(planner) => Ok(Some(planner)),
                Err(error) => {
                    tracing::info!("Falling back to the legacy query planner: {error}");
                    Ok(None)
                }
            },
        }
    }

    fn rust(
        schema: &Schema,
        configuration: &Configuration,
    ) -> Result<Arc<QueryPlanner>, ServiceBuildError> {
        let config = apollo_federation::query_plan::query_planner::QueryPlannerConfig {
            reuse_query_fragments: configuration
                .supergraph
                .reuse_query_fragments
                .unwrap_or(true),
            subgraph_graphql_validation: false,
            generate_query_fragments: configuration.supergraph.generate_query_fragments,
            incremental_delivery:
                apollo_federation::query_plan::query_planner::QueryPlanIncrementalDeliveryConfig {
                    enable_defer: configuration.supergraph.defer_support,
                },
            type_conditioned_fetching: configuration.experimental_type_conditioned_fetching,
            debug: Default::default(),
        };
        let result = QueryPlanner::new(schema.federation_supergraph(), config);

        match &result {
            Err(FederationError::SingleFederationError {
                inner: error,
                trace: _,
            }) => match error {
                SingleFederationError::UnsupportedFederationVersion { .. } => {
                    metric_rust_qp_init(Some(UNSUPPORTED_FED1));
                }
                SingleFederationError::UnsupportedFeature { message: _, kind } => match kind {
                    apollo_federation::error::UnsupportedFeatureKind::ProgressiveOverrides => {
                        metric_rust_qp_init(Some(UNSUPPORTED_OVERRIDES))
                    }
                    apollo_federation::error::UnsupportedFeatureKind::Context => {
                        metric_rust_qp_init(Some(UNSUPPORTED_CONTEXT))
                    }
                    _ => metric_rust_qp_init(Some(INTERNAL_INIT_ERROR)),
                },
                _ => {
                    metric_rust_qp_init(Some(INTERNAL_INIT_ERROR));
                }
            },
            Err(_) => metric_rust_qp_init(Some(INTERNAL_INIT_ERROR)),
            Ok(_) => metric_rust_qp_init(None),
        }

        Ok(Arc::new(result.map_err(ServiceBuildError::QpInitError)?))
    }

    async fn js_planner(
        sdl: &str,
        configuration: &Configuration,
        old_js_planner: &Option<Arc<Planner<QueryPlanResult>>>,
    ) -> Result<Arc<Planner<QueryPlanResult>>, ServiceBuildError> {
        let query_planner_configuration = configuration.js_query_planner_config();
        let planner = match old_js_planner {
            None => Planner::new(sdl.to_owned(), query_planner_configuration).await?,
            Some(old_planner) => {
                old_planner
                    .update(sdl.to_owned(), query_planner_configuration)
                    .await?
            }
        };
        Ok(Arc::new(planner))
    }

    async fn js_introspection(
        &self,
        sdl: &str,
        configuration: &Configuration,
        old_js_planner: &Option<Arc<Planner<QueryPlanResult>>>,
        cache: CacheStorage<String, Response>,
    ) -> Result<Arc<Introspection>, ServiceBuildError> {
        let js_planner = match self {
            Self::Js(js) => js.clone(),
            Self::Both { js, .. } => js.clone(),
            Self::Rust(_) => {
                // JS "planner" (actually runtime) was not created for planning
                // but is still needed for introspection, so create it now
                Self::js_planner(sdl, configuration, old_js_planner).await?
            }
        };
        Ok(Arc::new(
            Introspection::with_cache(js_planner, cache).await?,
        ))
    }

    async fn plan(
        &self,
        doc: &ParsedDocument,
        filtered_query: String,
        operation: Option<String>,
        plan_options: PlanOptions,
        // Initialization code that needs mutable access to the plan,
        // before we potentially share it in Arc with a background thread
        // for "both" mode.
        init_query_plan_root_node: impl Fn(&mut PlanNode) -> Result<(), ValidationErrors>,
    ) -> Result<PlanSuccess<QueryPlanResult>, QueryPlannerError> {
        match self {
            PlannerMode::Js(js) => {
                let start = Instant::now();

                let result = js.plan(filtered_query, operation, plan_options).await;

                let elapsed = start.elapsed().as_secs_f64();
                metric_query_planning_plan_duration(JS_QP_MODE, elapsed);

                let mut success = result
                    .map_err(QueryPlannerError::RouterBridgeError)?
                    .into_result()
                    .map_err(PlanErrors::from)?;

                if let Some(root_node) = &mut success.data.query_plan.node {
                    // Arc freshly deserialized from Deno should be unique, so this doesn’t clone:
                    let root_node = Arc::make_mut(root_node);
                    init_query_plan_root_node(root_node)?;
                }
                Ok(success)
            }
            PlannerMode::Rust(rust_planner) => {
                let doc = doc.clone();
                let rust_planner = rust_planner.clone();
                let (plan, mut root_node) = tokio::task::spawn_blocking(move || {
                    let start = Instant::now();

                    let result = operation
                        .as_deref()
                        .map(|n| Name::new(n).map_err(FederationError::from))
                        .transpose()
                        .and_then(|operation| {
                            rust_planner.build_query_plan(&doc.executable, operation)
                        })
                        .map_err(|e| QueryPlannerError::FederationError(e.to_string()));

                    let elapsed = start.elapsed().as_secs_f64();
                    metric_query_planning_plan_duration(RUST_QP_MODE, elapsed);

                    result.map(|plan| {
                        let root_node = convert_root_query_plan_node(&plan);
                        (plan, root_node)
                    })
                })
                .await
                .expect("query planner panicked")?;
                if let Some(node) = &mut root_node {
                    init_query_plan_root_node(node)?;
                }

                // Dummy value overwritten below in `BrigeQueryPlanner::plan`
                let usage_reporting = UsageReporting {
                    stats_report_key: Default::default(),
                    referenced_fields_by_type: Default::default(),
                };

                Ok(PlanSuccess {
                    usage_reporting,
                    data: QueryPlanResult {
                        formatted_query_plan: Some(Arc::new(plan.to_string())),
                        query_plan: QueryPlan {
                            node: root_node.map(Arc::new),
                        },
                        evaluated_plan_count: plan
                            .statistics
                            .evaluated_plan_count
                            .clone()
                            .into_inner() as u64,
                    },
                })
            }
            PlannerMode::Both { js, rust } => {
                let start = Instant::now();

                let result = js
                    .plan(filtered_query, operation.clone(), plan_options)
                    .await;

                let elapsed = start.elapsed().as_secs_f64();
                metric_query_planning_plan_duration(JS_QP_MODE, elapsed);

                let mut js_result = result
                    .map_err(QueryPlannerError::RouterBridgeError)?
                    .into_result()
                    .map_err(PlanErrors::from);

                if let Ok(success) = &mut js_result {
                    if let Some(root_node) = &mut success.data.query_plan.node {
                        // Arc freshly deserialized from Deno should be unique, so this doesn’t clone:
                        let root_node = Arc::make_mut(root_node);
                        init_query_plan_root_node(root_node)?;
                    }
                }

                BothModeComparisonJob {
                    rust_planner: rust.clone(),
                    js_duration: elapsed,
                    document: doc.executable.clone(),
                    operation_name: operation,
                    // Exclude usage reporting from the Result sent for comparison
                    js_result: js_result
                        .as_ref()
                        .map(|success| success.data.clone())
                        .map_err(|e| e.errors.clone()),
                }
                .schedule();

                Ok(js_result?)
            }
        }
    }

    async fn subgraphs(
        &self,
    ) -> Result<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>, ServiceBuildError> {
        let js = match self {
            PlannerMode::Js(js) => js,
            PlannerMode::Both { js, .. } => js,
            PlannerMode::Rust(rust) => {
                return Ok(rust
                    .subgraph_schemas()
                    .iter()
                    .map(|(name, schema)| (name.to_string(), Arc::new(schema.schema().clone())))
                    .collect())
            }
        };
        js.subgraphs()
            .await?
            .into_iter()
            .map(|(name, schema_str)| {
                let schema = apollo_compiler::Schema::parse_and_validate(schema_str, "")
                    .map_err(|errors| SchemaError::Validate(errors.into()))?;
                Ok((name, Arc::new(schema)))
            })
            .collect()
    }
}

impl BridgeQueryPlanner {
    pub(crate) async fn new(
        schema: Arc<Schema>,
        configuration: Arc<Configuration>,
        old_js_planner: Option<Arc<Planner<QueryPlanResult>>>,
        rust_planner: Option<Arc<QueryPlanner>>,
        cache: CacheStorage<String, Response>,
    ) -> Result<Self, ServiceBuildError> {
        let planner =
            PlannerMode::new(&schema, &configuration, &old_js_planner, rust_planner).await?;

        let subgraph_schemas = Arc::new(planner.subgraphs().await?);

        let introspection = if configuration.supergraph.introspection {
            match configuration.experimental_introspection_mode {
                IntrospectionConfig::New => IntrospectionMode::Rust,
                IntrospectionConfig::Legacy => IntrospectionMode::Js(
                    planner
                        .js_introspection(&schema.raw_sdl, &configuration, &old_js_planner, cache)
                        .await?,
                ),
                IntrospectionConfig::Both => IntrospectionMode::Both(
                    planner
                        .js_introspection(&schema.raw_sdl, &configuration, &old_js_planner, cache)
                        .await?,
                ),
            }
        } else {
            IntrospectionMode::Disabled
        };

        let enable_authorization_directives =
            AuthorizationPlugin::enable_directives(&configuration, &schema)?;
        let federation_instrument = federation_version_instrument(schema.federation_version());
        let signature_normalization_algorithm =
            TelemetryConfig::signature_normalization_algorithm(&configuration);

        Ok(Self {
            planner,
            schema,
            subgraph_schemas,
            introspection,
            enable_authorization_directives,
            configuration,
            _federation_instrument: federation_instrument,
            signature_normalization_algorithm,
        })
    }

    pub(crate) fn js_planner(&self) -> Option<Arc<Planner<QueryPlanResult>>> {
        match &self.planner {
            PlannerMode::Js(js) => Some(js.clone()),
            PlannerMode::Both { js, .. } => Some(js.clone()),
            PlannerMode::Rust(_) => match &self.introspection {
                IntrospectionMode::Js(js_introspection)
                | IntrospectionMode::Both(js_introspection) => {
                    Some(js_introspection.planner.clone())
                }
                IntrospectionMode::Rust | IntrospectionMode::Disabled => None,
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn schema(&self) -> Arc<Schema> {
        self.schema.clone()
    }

    pub(crate) fn subgraph_schemas(
        &self,
    ) -> Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>> {
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

        let (fragments, operations, defer_stats, schema_aware_hash) =
            Query::extract_query_information(&self.schema, executable, operation_name)?;

        let subselections = crate::spec::query::subselections::collect_subselections(
            &self.configuration,
            &operations,
            &fragments.map,
            &defer_stats,
        )?;
        Ok(Query {
            string: query,
            fragments,
            operations,
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

    async fn introspection(
        &self,
        key: QueryKey,
        doc: ParsedDocument,
    ) -> Result<QueryPlannerContent, QueryPlannerError> {
        match &self.introspection {
            IntrospectionMode::Disabled => return Ok(QueryPlannerContent::IntrospectionDisabled),
            IntrospectionMode::Rust => {
                let schema = self.schema.clone();
                let response = Box::new(
                    tokio::task::spawn_blocking(move || {
                        Self::rust_introspection(&schema, &key, &doc)
                    })
                    .await
                    .expect("Introspection panicked")?,
                );
                return Ok(QueryPlannerContent::Response { response });
            }
            IntrospectionMode::Js(_) | IntrospectionMode::Both(_) => {}
        }

        if doc.executable.operations.len() > 1 {
            // TODO: add an operation_name parameter to router-bridge to fix this?
            let error = graphql::Error::builder()
                .message(
                    "Schema introspection is currently not supported \
                     with multiple operations in the same document",
                )
                .extension_code("INTROSPECTION_WITH_MULTIPLE_OPERATIONS")
                .build();
            return Ok(QueryPlannerContent::Response {
                response: Box::new(graphql::Response::builder().error(error).build()),
            });
        }

        let response = match &self.introspection {
            IntrospectionMode::Rust | IntrospectionMode::Disabled => unreachable!(), // returned above
            IntrospectionMode::Js(js) => js
                .execute(key.filtered_query)
                .await
                .map_err(QueryPlannerError::Introspection)?,
            IntrospectionMode::Both(js) => {
                let js_result = js
                    .execute(key.filtered_query.clone())
                    .await
                    .map_err(QueryPlannerError::Introspection);
                let schema = self.schema.clone();
                let js_result_clone = js_result.clone();
                tokio::task::spawn_blocking(move || {
                    let rust_result = match Self::rust_introspection(&schema, &key, &doc) {
                        Ok(response) => {
                            if response.errors.is_empty() {
                                Ok(response)
                            } else {
                                Err(QueryPlannerError::Introspection(IntrospectionError {
                                    message: Some(
                                        response
                                            .errors
                                            .into_iter()
                                            .map(|e| e.to_string())
                                            .collect::<Vec<_>>()
                                            .join(", "),
                                    ),
                                }))
                            }
                        }
                        Err(e) => Err(e),
                    };
                    super::dual_introspection::compare_introspection_responses(
                        &key.original_query,
                        js_result_clone,
                        rust_result,
                    );
                })
                .await
                .expect("Introspection comparison panicked");
                js_result?
            }
        };
        Ok(QueryPlannerContent::Response {
            response: Box::new(response),
        })
    }

    fn rust_introspection(
        schema: &Schema,
        key: &QueryKey,
        doc: &ParsedDocument,
    ) -> Result<graphql::Response, QueryPlannerError> {
        let schema = schema.api_schema();
        let operation = doc.get_operation(key.operation_name.as_deref())?;
        let variable_values = Default::default();
        let variable_values =
            apollo_compiler::execution::coerce_variable_values(schema, operation, &variable_values)
                .map_err(|e| {
                    let message = match &e {
                        InputCoercionError::SuspectedValidationBug(e) => &e.message,
                        InputCoercionError::ValueError { message, .. } => message,
                    };
                    QueryPlannerError::Introspection(IntrospectionError {
                        message: Some(message.clone()),
                    })
                })?;
        let response = apollo_compiler::execution::execute_introspection_only_query(
            schema,
            &doc.executable,
            operation,
            &variable_values,
        );
        Ok(response.into())
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
        query_metrics: OperationLimits<u32>,
    ) -> Result<QueryPlannerContent, QueryPlannerError> {
        let plan_success = self
            .planner
            .plan(
                doc,
                filtered_query.clone(),
                operation.clone(),
                plan_options,
                |root_node| {
                    root_node.init_parsed_operations_and_hash_subqueries(
                        &self.subgraph_schemas,
                        &self.schema.raw_sdl,
                    )?;
                    root_node.extract_authorization_metadata(self.schema.supergraph_schema(), &key);
                    Ok(())
                },
            )
            .await?;

        match plan_success {
            PlanSuccess {
                data:
                    QueryPlanResult {
                        query_plan: QueryPlan { node: Some(node) },
                        formatted_query_plan,
                        evaluated_plan_count,
                    },
                mut usage_reporting,
            } => {
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

                u64_histogram!(
                    "apollo.router.query_planning.plan.evaluated_plans",
                    "Number of query plans evaluated for a query before choosing the best one",
                    evaluated_plan_count
                );

                let generated_usage_reporting = generate_usage_reporting(
                    &signature_doc.executable,
                    &doc.executable,
                    &operation,
                    self.schema.supergraph_schema(),
                    &self.signature_normalization_algorithm,
                );

                usage_reporting.stats_report_key =
                    generated_usage_reporting.result.stats_report_key;
                usage_reporting.referenced_fields_by_type =
                    generated_usage_reporting.result.referenced_fields_by_type;

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
            }
            #[cfg_attr(feature = "failfast", allow(unused_variables))]
            PlanSuccess {
                data:
                    QueryPlanResult {
                        query_plan: QueryPlan { node: None },
                        ..
                    },
                usage_reporting,
            } => {
                failfast_debug!("empty query plan");
                Err(QueryPlannerError::EmptyPlan(usage_reporting))
            }
        }
    }
}

impl Service<QueryPlannerRequest> for BridgeQueryPlanner {
    type Response = QueryPlannerResponse;

    type Error = QueryPlannerError;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: QueryPlannerRequest) -> Self::Future {
        let QueryPlannerRequest {
            query: original_query,
            operation_name,
            context,
        } = req;

        let metadata = context
            .extensions()
            .with_lock(|lock| lock.get::<CacheKeyMetadata>().cloned().unwrap_or_default());
        let this = self.clone();
        let fut = async move {
            let mut doc = match context
                .extensions()
                .with_lock(|lock| lock.get::<ParsedDocument>().cloned())
            {
                None => return Err(QueryPlannerError::SpecError(SpecError::UnknownFileId)),
                Some(d) => d,
            };

            let api_schema = this.schema.api_schema();
            match add_defer_labels(api_schema, &doc.ast) {
                Err(e) => {
                    return Err(QueryPlannerError::SpecError(SpecError::TransformError(
                        e.to_string(),
                    )))
                }
                Ok(modified_query) => {
                    let executable_document = modified_query
                        .to_executable_validate(api_schema)
                        // Assume transformation creates a valid document: ignore conversion errors
                        .map_err(|e| SpecError::ValidationError(e.into()))?;
                    let hash = QueryHashVisitor::hash_query(
                        this.schema.supergraph_schema(),
                        &this.schema.raw_sdl,
                        &executable_document,
                        operation_name.as_deref(),
                    )
                    .map_err(|e| SpecError::QueryHashing(e.to_string()))?;
                    doc = Arc::new(ParsedDocumentInner {
                        executable: Arc::new(executable_document),
                        ast: modified_query,
                        hash: Arc::new(QueryHash(hash)),
                    });
                    context
                        .extensions()
                        .with_lock(|mut lock| lock.insert::<ParsedDocument>(doc.clone()));
                }
            }

            let plan_options = PlanOptions {
                override_conditions: context
                    .get(LABELS_TO_OVERRIDE_KEY)
                    .unwrap_or_default()
                    .unwrap_or_default(),
            };

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
                )
                .await;

            match res {
                Ok(query_planner_content) => Ok(QueryPlannerResponse::builder()
                    .content(query_planner_content)
                    .context(context)
                    .build()),
                Err(e) => {
                    match &e {
                        QueryPlannerError::PlanningErrors(pe) => {
                            context.extensions().with_lock(|mut lock| {
                                lock.insert(Arc::new(pe.usage_reporting.clone()))
                            });
                        }
                        QueryPlannerError::SpecError(e) => {
                            context.extensions().with_lock(|mut lock| {
                                lock.insert(Arc::new(UsageReporting {
                                    stats_report_key: e.get_error_key().to_string(),
                                    referenced_fields_by_type: HashMap::new(),
                                }))
                            });
                        }
                        _ => (),
                    }
                    Err(e)
                }
            }
        };

        // Return the response as an immediate future
        Box::pin(fut)
    }
}

// Appease clippy::type_complexity
pub(crate) type FilteredQuery = (Vec<Path>, ast::Document);

impl BridgeQueryPlanner {
    async fn get(
        &self,
        mut key: QueryKey,
        mut doc: ParsedDocument,
    ) -> Result<QueryPlannerContent, QueryPlannerError> {
        let mut query_metrics = Default::default();
        let mut selections = self
            .parse_selections(
                key.original_query.clone(),
                key.operation_name.as_deref(),
                &doc,
                &mut query_metrics,
            )
            .await?;

        if selections
            .operation(key.operation_name.as_deref())
            .is_some_and(|op| op.selection_set.is_empty())
        {
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

        {
            let operation = doc
                .executable
                .operations
                .get(key.operation_name.as_deref())
                .ok();
            let mut has_root_typename = false;
            let mut has_schema_introspection = false;
            let mut has_other_root_fields = false;
            if let Some(operation) = operation {
                for field in operation.root_fields(&doc.executable) {
                    match field.name.as_str() {
                        "__typename" => has_root_typename = true,
                        "__schema" | "__type" if operation.is_query() => {
                            has_schema_introspection = true
                        }
                        _ => has_other_root_fields = true,
                    }
                }
                if has_root_typename && !has_schema_introspection && !has_other_root_fields {
                    // Fast path for __typename alone
                    if operation
                        .selection_set
                        .selections
                        .iter()
                        .all(|sel| sel.as_field().is_some_and(|f| f.name == "__typename"))
                    {
                        let root_type_name: serde_json_bytes::ByteString =
                            operation.object_type().as_str().into();
                        let data = Value::Object(
                            operation
                                .root_fields(&doc.executable)
                                .filter(|field| field.name == "__typename")
                                .map(|field| {
                                    (
                                        field.response_key().as_str().into(),
                                        Value::String(root_type_name.clone()),
                                    )
                                })
                                .collect(),
                        );
                        return Ok(QueryPlannerContent::Response {
                            response: Box::new(graphql::Response::builder().data(data).build()),
                        });
                    } else {
                        // fragments might use @include or @skip
                    }
                }
            } else {
                // Should be unreachable as QueryAnalysisLayer would have returned an error
            }

            if has_schema_introspection {
                if has_other_root_fields {
                    let error = graphql::Error::builder()
                    .message("Mixed queries with both schema introspection and concrete fields are not supported")
                    .extension_code("MIXED_INTROSPECTION")
                    .build();
                    return Ok(QueryPlannerContent::Response {
                        response: Box::new(graphql::Response::builder().error(error).build()),
                    });
                }
                return self.introspection(key, doc).await;
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
            key.filtered_query = new_doc.to_string();
            let executable_document = new_doc
                .to_executable_validate(self.schema.api_schema())
                .map_err(|e| SpecError::ValidationError(e.into()))?;
            let hash = QueryHashVisitor::hash_query(
                self.schema.supergraph_schema(),
                &self.schema.raw_sdl,
                &executable_document,
                key.operation_name.as_deref(),
            )
            .map_err(|e| SpecError::QueryHashing(e.to_string()))?;
            doc = Arc::new(ParsedDocumentInner {
                executable: Arc::new(executable_document),
                ast: new_doc,
                hash: Arc::new(QueryHash(hash)),
            });
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
            query_metrics,
        )
        .await
    }
}

/// Data coming from the `plan` method on the router_bridge
// Note: Reexported under `apollo_router::_private`
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryPlanResult {
    pub(super) formatted_query_plan: Option<Arc<String>>,
    pub(super) query_plan: QueryPlan,
    pub(super) evaluated_plan_count: u64,
}

impl QueryPlanResult {
    pub fn formatted_query_plan(&self) -> Option<&str> {
        self.formatted_query_plan.as_deref().map(String::as_str)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
/// The root query plan container.
pub(super) struct QueryPlan {
    /// The hierarchical nodes that make up the query plan
    pub(super) node: Option<Arc<PlanNode>>,
}

// Note: Reexported under `apollo_router::_private`
pub fn render_diff(differences: &[diff::Result<&str>]) -> String {
    let mut output = String::new();
    for diff_line in differences {
        match diff_line {
            diff::Result::Left(l) => {
                let trimmed = l.trim();
                if !trimmed.starts_with('#') && !trimmed.is_empty() {
                    writeln!(&mut output, "-{l}").expect("write will never fail");
                } else {
                    writeln!(&mut output, " {l}").expect("write will never fail");
                }
            }
            diff::Result::Both(l, _) => {
                writeln!(&mut output, " {l}").expect("write will never fail");
            }
            diff::Result::Right(r) => {
                let trimmed = r.trim();
                if trimmed != "---" && !trimmed.is_empty() {
                    writeln!(&mut output, "+{r}").expect("write will never fail");
                }
            }
        }
    }
    output
}

pub(crate) fn metric_query_planning_plan_duration(planner: &'static str, elapsed: f64) {
    f64_histogram!(
        "apollo.router.query_planning.plan.duration",
        "Duration of the query planning.",
        elapsed,
        "planner" = planner
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
    use std::fs;
    use std::path::PathBuf;

    use serde_json::json;
    use test_log::test;
    use tower::Service;
    use tower::ServiceExt;

    use super::*;
    use crate::introspection::default_cache_storage;
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

    #[test]
    fn empty_query_plan() {
        serde_json::from_value::<QueryPlan>(json!({ "plan": { "kind": "QueryPlan"} } )).expect(
            "If this test fails, It probably means QueryPlan::node isn't an Option anymore.\n
                 Introspection queries return an empty QueryPlan, so the node field needs to remain optional.",
        );
    }

    #[test(tokio::test)]
    async fn federation_versions() {
        async {
            let sdl = include_str!("../testdata/minimal_fed1_supergraph.graphql");
            let config = Arc::default();
            let schema = Schema::parse(sdl, &config).unwrap();
            let _planner = BridgeQueryPlanner::new(
                schema.into(),
                config,
                None,
                None,
                default_cache_storage().await,
            )
            .await
            .unwrap();

            assert_gauge!(
                "apollo.router.supergraph.federation",
                1,
                federation.version = 1
            );
        }
        .with_metrics()
        .await;

        async {
            let sdl = include_str!("../testdata/minimal_supergraph.graphql");
            let config = Arc::default();
            let schema = Schema::parse(sdl, &config).unwrap();
            let _planner = BridgeQueryPlanner::new(
                schema.into(),
                config,
                None,
                None,
                default_cache_storage().await,
            )
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
        let schema = Arc::new(Schema::parse(EXAMPLE_SCHEMA, &Default::default()).unwrap());
        let query = include_str!("testdata/unknown_introspection_query.graphql");

        let planner = BridgeQueryPlanner::new(
            schema.clone(),
            Default::default(),
            None,
            None,
            default_cache_storage().await,
        )
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
                query_metrics
            )
            .await
            .unwrap_err();

        match err {
            QueryPlannerError::EmptyPlan(usage_reporting) => {
                insta::with_settings!({sort_maps => true}, {
                    insta::assert_json_snapshot!("empty_query_plan_usage_reporting", usage_reporting);
                });
            }
            e => {
                panic!("empty plan should have returned an EmptyPlanError: {e:?}");
            }
        }
    }

    #[test(tokio::test)]
    async fn test_plan_error() {
        let result = plan(EXAMPLE_SCHEMA, "", "", None, PlanOptions::default()).await;

        assert_eq!(
            "couldn't plan query: query validation errors: Syntax Error: Unexpected <EOF>.",
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
        if let QueryPlannerContent::Response { response } = result {
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
        if let QueryPlannerContent::Response { response } = result {
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
        let planner = BridgeQueryPlanner::new(
            schema.into(),
            configuration.clone(),
            None,
            None,
            default_cache_storage().await,
        )
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
        // FIXME: uncomment myName alias when this is fixed:
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

    async fn subselections_keys(query: &str, planner: &BridgeQueryPlanner) -> String {
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
        let planner = BridgeQueryPlanner::new(
            schema.into(),
            configuration.clone(),
            None,
            None,
            default_cache_storage().await,
        )
        .await
        .unwrap();

        let doc = Query::parse_document(
            original_query,
            operation_name.as_deref(),
            &planner.schema(),
            &configuration,
        )
        .unwrap();

        planner
            .get(
                QueryKey {
                    original_query: original_query.to_string(),
                    filtered_query: filtered_query.to_string(),
                    operation_name,
                    metadata: CacheKeyMetadata::default(),
                    plan_options,
                },
                doc,
            )
            .await
    }

    #[test]
    fn router_bridge_dependency_is_pinned() {
        let cargo_manifest: serde_json::Value = basic_toml::from_str(
            &fs::read_to_string(PathBuf::from(&env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
                .expect("could not read Cargo.toml"),
        )
        .expect("could not parse Cargo.toml");
        let router_bridge_version = cargo_manifest
            .get("dependencies")
            .expect("Cargo.toml does not contain dependencies")
            .as_object()
            .expect("Cargo.toml dependencies key is not an object")
            .get("router-bridge")
            .expect("Cargo.toml dependencies does not have an entry for router-bridge")
            .as_str()
            .unwrap_or_default();
        assert!(
            router_bridge_version.contains('='),
            "router-bridge in Cargo.toml is not pinned with a '=' prefix"
        );
    }

    #[tokio::test]
    async fn test_both_mode() {
        let mut harness = crate::TestHarness::builder()
            // auth is not relevant here, but supergraph.graphql uses join/v0.1
            // which is not supported by the Rust query planner
            .schema(include_str!("../../tests/fixtures/supergraph-auth.graphql"))
            .configuration_json(serde_json::json!({
                "experimental_query_planner_mode": "both",
            }))
            .unwrap()
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
    }

    #[tokio::test]
    async fn test_rust_mode_subgraph_operation_serialization() {
        let subgraph_queries = Arc::new(tokio::sync::Mutex::new(String::new()));
        let subgraph_queries2 = Arc::clone(&subgraph_queries);
        let mut harness = crate::TestHarness::builder()
            // auth is not relevant here, but supergraph.graphql uses join/v0.1
            // which is not supported by the Rust query planner
            .schema(include_str!("../../tests/fixtures/supergraph-auth.graphql"))
            .configuration_json(serde_json::json!({
                "experimental_query_planner_mode": "new",
            }))
            .unwrap()
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
        metric_query_planning_plan_duration(RUST_QP_MODE, elapsed);
        assert_histogram_exists!(
            "apollo.router.query_planning.plan.duration",
            f64,
            "planner" = "rust"
        );

        let start = Instant::now();
        let elapsed = start.elapsed().as_secs_f64();
        metric_query_planning_plan_duration(JS_QP_MODE, elapsed);
        assert_histogram_exists!(
            "apollo.router.query_planning.plan.duration",
            f64,
            "planner" = "js"
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
        metric_rust_qp_init(Some(UNSUPPORTED_CONTEXT));
        assert_counter!(
            "apollo.router.lifecycle.query_planner.init",
            1,
            "init.error_kind" = "context",
            "init.is_success" = false
        );
        metric_rust_qp_init(Some(UNSUPPORTED_OVERRIDES));
        assert_counter!(
            "apollo.router.lifecycle.query_planner.init",
            1,
            "init.error_kind" = "overrides",
            "init.is_success" = false
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
