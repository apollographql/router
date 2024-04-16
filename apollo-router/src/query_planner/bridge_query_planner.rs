//! Calls out to nodejs query planner

use std::collections::HashMap;
use std::fmt::Debug;
use std::fmt::Write;
use std::sync::Arc;

use apollo_compiler::ast;
use apollo_compiler::validation::Valid;
use futures::future::BoxFuture;
use opentelemetry_api::metrics::MeterProvider as _;
use opentelemetry_api::metrics::ObservableGauge;
use opentelemetry_api::KeyValue;
use router_bridge::planner::IncrementalDeliverySupport;
use router_bridge::planner::PlanOptions;
use router_bridge::planner::PlanSuccess;
use router_bridge::planner::Planner;
use router_bridge::planner::QueryPlannerConfig;
use router_bridge::planner::QueryPlannerDebugConfig;
use router_bridge::planner::UsageReporting;
use serde::Deserialize;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use tower::Service;

use super::PlanNode;
use super::QueryKey;
use crate::apollo_studio_interop::generate_usage_reporting;
use crate::apollo_studio_interop::UsageReportingComparisonResult;
use crate::configuration::ApolloMetricsGenerationMode;
use crate::error::PlanErrors;
use crate::error::QueryPlannerError;
use crate::error::SchemaError;
use crate::error::ServiceBuildError;
use crate::graphql;
use crate::introspection::Introspection;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::metrics::meter_provider;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::plugins::authorization::UnauthorizedPaths;
use crate::plugins::progressive_override::LABELS_TO_OVERRIDE_KEY;
use crate::query_planner::fetch::QueryHash;
use crate::query_planner::labeler::add_defer_labels;
use crate::services::layers::query_analysis::ParsedDocument;
use crate::services::layers::query_analysis::ParsedDocumentInner;
use crate::services::QueryPlannerContent;
use crate::services::QueryPlannerRequest;
use crate::services::QueryPlannerResponse;
use crate::spec::query::change::QueryHashVisitor;
use crate::spec::Query;
use crate::spec::Schema;
use crate::spec::SpecError;
use crate::Configuration;

#[derive(Clone)]
/// A query planner that calls out to the nodejs router-bridge query planner.
///
/// No caching is performed. To cache, wrap in a [`CachingQueryPlanner`].
pub(crate) struct BridgeQueryPlanner {
    planner: Arc<Planner<QueryPlanResult>>,
    schema: Arc<Schema>,
    subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
    introspection: Option<Arc<Introspection>>,
    configuration: Arc<Configuration>,
    enable_authorization_directives: bool,
    _federation_instrument: ObservableGauge<u64>,
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

impl BridgeQueryPlanner {
    pub(crate) async fn new(
        sdl: String,
        configuration: Arc<Configuration>,
    ) -> Result<Self, ServiceBuildError> {
        let schema = Schema::parse(&sdl)?;

        let planner = Planner::new(
            sdl,
            QueryPlannerConfig {
                reuse_query_fragments: configuration.supergraph.reuse_query_fragments,
                generate_query_fragments: Some(configuration.supergraph.generate_query_fragments),
                incremental_delivery: Some(IncrementalDeliverySupport {
                    enable_defer: Some(configuration.supergraph.defer_support),
                }),
                graphql_validation: false,
                debug: Some(QueryPlannerDebugConfig {
                    bypass_planner_for_single_subgraph: None,
                    max_evaluated_plans: configuration
                        .supergraph
                        .query_planning
                        .experimental_plans_limit
                        .or(Some(10000)),
                    paths_limit: configuration
                        .supergraph
                        .query_planning
                        .experimental_paths_limit,
                }),
                type_conditioned_fetching: configuration.experimental_type_conditioned_fetching,
            },
        )
        .await?;

        let planner = Arc::new(planner);

        let api_schema_string = match configuration.experimental_api_schema_generation_mode {
            crate::configuration::ApiSchemaMode::Legacy => {
                let api_schema = planner.api_schema().await?;
                api_schema.schema
            }
            crate::configuration::ApiSchemaMode::New => schema.create_api_schema(&configuration)?,

            crate::configuration::ApiSchemaMode::Both => {
                let api_schema = planner
                    .api_schema()
                    .await
                    .map(|api_schema| api_schema.schema);
                let new_api_schema = schema.create_api_schema(&configuration);

                match (&api_schema, &new_api_schema) {
                    (Err(js_error), Ok(_)) => {
                        tracing::warn!("JS API schema error: {}", js_error);
                        tracing::warn!(
                            monotonic_counter.apollo.router.lifecycle.api_schema = 1u64,
                            generation.is_matched = false,
                            "API schema generation mismatch: JS returns error but Rust does not"
                        );
                    }
                    (Ok(_), Err(rs_error)) => {
                        tracing::warn!("Rust API schema error: {}", rs_error);
                        tracing::warn!(
                            monotonic_counter.apollo.router.lifecycle.api_schema = 1u64,
                            generation.is_matched = false,
                            "API schema generation mismatch: JS returns API schema but Rust errors out"
                        );
                    }
                    (Ok(left), Ok(right)) => {
                        // To compare results, we re-parse, standardize, and print with apollo-rs,
                        // so the formatting is identical.
                        if let (Ok(left), Ok(right)) = (
                            apollo_compiler::Schema::parse(left, "js.graphql"),
                            apollo_compiler::Schema::parse(right, "rust.graphql"),
                        ) {
                            let left = standardize_schema(left).to_string();
                            let right = standardize_schema(right).to_string();

                            if left == right {
                                tracing::warn!(
                                    monotonic_counter.apollo.router.lifecycle.api_schema = 1u64,
                                    generation.is_matched = true,
                                );
                            } else {
                                tracing::warn!(
                                    monotonic_counter.apollo.router.lifecycle.api_schema = 1u64,
                                    generation.is_matched = false,
                                    "API schema generation mismatch: apollo-federation and router-bridge write different schema"
                                );

                                let differences = diff::lines(&left, &right);
                                tracing::debug!(
                                    "different API schema between apollo-federation and router-bridge:\n{}",
                                    render_diff(&differences),
                                );
                            }
                        }
                    }
                    (Err(_), Err(_)) => {
                        tracing::warn!(
                            monotonic_counter.apollo.router.lifecycle.api_schema = 1u64,
                            generation.is_matched = true,
                        );
                    }
                }

                api_schema?
            }
        };
        let api_schema = Schema::parse(&api_schema_string)?;

        let schema = Arc::new(schema.with_api_schema(api_schema));

        let mut subgraph_schemas: HashMap<String, Arc<Valid<apollo_compiler::Schema>>> =
            HashMap::new();
        for (name, schema_str) in planner.subgraphs().await? {
            let schema = apollo_compiler::Schema::parse_and_validate(schema_str, "")
                .map_err(|errors| SchemaError::Validate(errors.into()))?;
            subgraph_schemas.insert(name, Arc::new(schema));
        }
        let subgraph_schemas = Arc::new(subgraph_schemas);

        let introspection = if configuration.supergraph.introspection {
            Some(Arc::new(Introspection::new(planner.clone()).await?))
        } else {
            None
        };

        let enable_authorization_directives =
            AuthorizationPlugin::enable_directives(&configuration, &schema)?;
        let federation_instrument = federation_version_instrument(schema.federation_version());
        Ok(Self {
            planner,
            schema,
            subgraph_schemas,
            introspection,
            enable_authorization_directives,
            configuration,
            _federation_instrument: federation_instrument,
        })
    }

    pub(crate) async fn new_from_planner(
        old_planner: Arc<Planner<QueryPlanResult>>,
        schema: String,
        configuration: Arc<Configuration>,
    ) -> Result<Self, ServiceBuildError> {
        let planner = Arc::new(
            old_planner
                .update(
                    schema.clone(),
                    QueryPlannerConfig {
                        incremental_delivery: Some(IncrementalDeliverySupport {
                            enable_defer: Some(configuration.supergraph.defer_support),
                        }),
                        graphql_validation: false,
                        reuse_query_fragments: configuration.supergraph.reuse_query_fragments,
                        generate_query_fragments: Some(
                            configuration.supergraph.generate_query_fragments,
                        ),
                        debug: Some(QueryPlannerDebugConfig {
                            bypass_planner_for_single_subgraph: None,
                            max_evaluated_plans: configuration
                                .supergraph
                                .query_planning
                                .experimental_plans_limit
                                .or(Some(10000)),
                            paths_limit: configuration
                                .supergraph
                                .query_planning
                                .experimental_paths_limit,
                        }),
                        type_conditioned_fetching: configuration
                            .experimental_type_conditioned_fetching,
                    },
                )
                .await?,
        );

        let api_schema = planner.api_schema().await?;
        let api_schema = Schema::parse(&api_schema.schema)?;
        let schema = Arc::new(Schema::parse(&schema)?.with_api_schema(api_schema));

        let mut subgraph_schemas: HashMap<String, Arc<Valid<apollo_compiler::Schema>>> =
            HashMap::new();
        for (name, schema_str) in planner.subgraphs().await? {
            let schema = apollo_compiler::Schema::parse_and_validate(schema_str, "")
                .map_err(|errors| SchemaError::Validate(errors.into()))?;
            subgraph_schemas.insert(name, Arc::new(schema));
        }
        let subgraph_schemas = Arc::new(subgraph_schemas);

        let introspection = if configuration.supergraph.introspection {
            Some(Arc::new(Introspection::new(planner.clone()).await?))
        } else {
            None
        };

        let enable_authorization_directives =
            AuthorizationPlugin::enable_directives(&configuration, &schema)?;
        let federation_instrument = federation_version_instrument(schema.federation_version());
        Ok(Self {
            planner,
            schema,
            subgraph_schemas,
            introspection,
            enable_authorization_directives,
            configuration,
            _federation_instrument: federation_instrument,
        })
    }

    pub(crate) fn planner(&self) -> Arc<Planner<QueryPlanResult>> {
        self.planner.clone()
    }

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
    ) -> Result<Query, QueryPlannerError> {
        let executable = &doc.executable;
        crate::spec::operation_limits::check(
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

    async fn introspection(&self, query: String) -> Result<QueryPlannerContent, QueryPlannerError> {
        match self.introspection.as_ref() {
            Some(introspection) => {
                let response = introspection
                    .execute(query)
                    .await
                    .map_err(QueryPlannerError::Introspection)?;

                Ok(QueryPlannerContent::Response {
                    response: Box::new(response),
                })
            }
            None => Ok(QueryPlannerContent::IntrospectionDisabled),
        }
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
    ) -> Result<QueryPlannerContent, QueryPlannerError> {
        let planner_result = match self
            .planner
            .plan(filtered_query.clone(), operation.clone(), plan_options)
            .await
            .map_err(QueryPlannerError::RouterBridgeError)?
            .into_result()
        {
            Ok(mut plan) => {
                plan.data.query_plan.hash_subqueries(&self.subgraph_schemas);
                plan.data
                    .query_plan
                    .extract_authorization_metadata(&self.schema.definitions, &key);
                plan
            }
            Err(err) => {
                let plan_errors: PlanErrors = err.into();
                return Err(QueryPlannerError::from(plan_errors));
            }
        };

        // the `statsReportKey` field should match the original query instead of the filtered query, to index them all under the same query
        let operation_signature = if matches!(
            self.configuration
                .experimental_apollo_metrics_generation_mode,
            ApolloMetricsGenerationMode::Legacy | ApolloMetricsGenerationMode::Both
        ) && original_query != filtered_query
        {
            Some(
                self.planner
                    .operation_signature(original_query.clone(), operation.clone())
                    .await
                    .map_err(QueryPlannerError::RouterBridgeError)?,
            )
        } else {
            None
        };

        match planner_result {
            PlanSuccess {
                data:
                    QueryPlanResult {
                        query_plan: QueryPlan { node: Some(node) },
                        formatted_query_plan,
                    },
                mut usage_reporting,
            } => {
                if let Some(sig) = operation_signature {
                    usage_reporting.stats_report_key = sig;
                }

                if matches!(
                    self.configuration
                        .experimental_apollo_metrics_generation_mode,
                    ApolloMetricsGenerationMode::New | ApolloMetricsGenerationMode::Both
                ) {
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

                    let generated_usage_reporting = generate_usage_reporting(
                        &signature_doc.executable,
                        &doc.executable,
                        &operation,
                        &self.schema.definitions,
                    );

                    // Ignore comparison if the operation name is an empty string since there is a known issue where
                    // router behaviour is incorrect in that case, and it also generates incorrect usage reports.
                    // https://github.com/apollographql/router/issues/4837
                    let is_empty_operation_name = operation.map_or(false, |s| s.is_empty());
                    let is_in_both_metrics_mode = matches!(
                        self.configuration
                            .experimental_apollo_metrics_generation_mode,
                        ApolloMetricsGenerationMode::Both
                    );
                    if !is_empty_operation_name && is_in_both_metrics_mode {
                        let comparison_result = generated_usage_reporting.compare(&usage_reporting);

                        if matches!(
                            comparison_result,
                            UsageReportingComparisonResult::StatsReportKeyNotEqual
                                | UsageReportingComparisonResult::BothNotEqual
                        ) {
                            tracing::warn!(
                                monotonic_counter.apollo.router.operations.telemetry.studio.signature = 1u64,
                                generation.is_matched = false,
                                "Mismatch between the Apollo usage reporting signature generated in router and router-bridge"
                            );
                            tracing::debug!(
                                "Different signatures generated between router and router-bridge:\n{}\n{}",
                                generated_usage_reporting.result.stats_report_key,
                                usage_reporting.stats_report_key,
                            );
                        } else {
                            tracing::info!(
                                monotonic_counter
                                    .apollo
                                    .router
                                    .operations
                                    .telemetry
                                    .studio
                                    .signature = 1u64,
                                generation.is_matched = true,
                            );
                        }

                        if matches!(
                            comparison_result,
                            UsageReportingComparisonResult::ReferencedFieldsNotEqual
                                | UsageReportingComparisonResult::BothNotEqual
                        ) {
                            tracing::warn!(
                                monotonic_counter.apollo.router.operations.telemetry.studio.references = 1u64,
                                generation.is_matched = false,
                                "Mismatch between the Apollo usage report referenced fields generated in router and router-bridge"
                            );
                            tracing::debug!(
                                "Different referenced fields generated between router and router-bridge:\n{:?}\n{:?}",
                                generated_usage_reporting.result.referenced_fields_by_type,
                                usage_reporting.referenced_fields_by_type,
                            );
                        } else {
                            tracing::info!(
                                monotonic_counter
                                    .apollo
                                    .router
                                    .operations
                                    .telemetry
                                    .studio
                                    .references = 1u64,
                                generation.is_matched = true,
                            );
                        }
                    } else if matches!(
                        self.configuration
                            .experimental_apollo_metrics_generation_mode,
                        ApolloMetricsGenerationMode::New
                    ) {
                        usage_reporting.stats_report_key =
                            generated_usage_reporting.result.stats_report_key;
                        usage_reporting.referenced_fields_by_type =
                            generated_usage_reporting.result.referenced_fields_by_type;
                    }
                }

                Ok(QueryPlannerContent::Plan {
                    plan: Arc::new(super::QueryPlan {
                        usage_reporting: Arc::new(usage_reporting),
                        root: node,
                        formatted_query_plan,
                        query: Arc::new(selections),
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
                mut usage_reporting,
            } => {
                failfast_debug!("empty query plan");
                if let Some(sig) = operation_signature {
                    usage_reporting.stats_report_key = sig;
                }

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
            .lock()
            .get::<CacheKeyMetadata>()
            .cloned()
            .unwrap_or_default();
        let this = self.clone();
        let fut = async move {
            let mut doc = match context.extensions().lock().get::<ParsedDocument>().cloned() {
                None => return Err(QueryPlannerError::SpecError(SpecError::UnknownFileId)),
                Some(d) => d,
            };

            let schema = &this.schema.api_schema().definitions;
            match add_defer_labels(schema, &doc.ast) {
                Err(e) => {
                    return Err(QueryPlannerError::SpecError(SpecError::TransformError(
                        e.to_string(),
                    )))
                }
                Ok(modified_query) => {
                    let executable_document = modified_query
                        .to_executable_validate(schema)
                        .map_err(|e| SpecError::ValidationError(e.into()))?;
                    let hash = QueryHashVisitor::hash_query(
                        schema,
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
                        .lock()
                        .insert::<ParsedDocument>(doc.clone());
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
                            context
                                .extensions()
                                .lock()
                                .insert(Arc::new(pe.usage_reporting.clone()));
                        }
                        QueryPlannerError::SpecError(e) => {
                            context.extensions().lock().insert(Arc::new(UsageReporting {
                                stats_report_key: e.get_error_key().to_string(),
                                referenced_fields_by_type: HashMap::new(),
                            }));
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

        let mut selections = self
            .parse_selections(
                key.original_query.clone(),
                key.operation_name.as_deref(),
                &doc,
            )
            .await?;

        if let Some((unauthorized_paths, new_doc)) = filter_res {
            key.filtered_query = new_doc.to_string();
            let executable_document = new_doc
                .to_executable_validate(&self.schema.api_schema().definitions)
                .map_err(|e| SpecError::ValidationError(e.into()))?;
            let hash = QueryHashVisitor::hash_query(
                &self.schema.definitions,
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

        if selections.contains_introspection() {
            // It can happen if you have a statically skipped query like { get @skip(if: true) { id name }} because it will be statically filtered with {}
            if selections
                .operations
                .first()
                .map(|op| op.selection_set.is_empty())
                .unwrap_or_default()
            {
                return Ok(QueryPlannerContent::Response {
                    response: Box::new(
                        graphql::Response::builder()
                            .data(Value::Object(Default::default()))
                            .build(),
                    ),
                });
            }
            // If we have only one operation containing only the root field `__typename`
            // (possibly aliased or repeated). (This does mean we fail to properly support
            // {"query": "query A {__typename} query B{somethingElse}", "operationName":"A"}.)
            if let Some(output_keys) = selections
                .operations
                .first()
                .and_then(|op| op.is_only_typenames_with_output_keys())
            {
                let operation_name = selections.operations[0].kind().to_string();
                let data: Value = Value::Object(Map::from_iter(
                    output_keys
                        .into_iter()
                        .map(|key| (key, Value::String(operation_name.clone().into()))),
                ));
                return Ok(QueryPlannerContent::Response {
                    response: Box::new(graphql::Response::builder().data(data).build()),
                });
            } else {
                return self.introspection(key.original_query).await;
            }
        }

        if key.filtered_query != key.original_query {
            let mut filtered = self
                .parse_selections(
                    key.filtered_query.clone(),
                    key.operation_name.as_deref(),
                    &doc,
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
        )
        .await
    }
}

/// Data coming from the `plan` method on the router_bridge
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueryPlanResult {
    formatted_query_plan: Option<String>,
    query_plan: QueryPlan,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
/// The root query plan container.
struct QueryPlan {
    /// The hierarchical nodes that make up the query plan
    node: Option<PlanNode>,
}

impl QueryPlan {
    fn hash_subqueries(&mut self, schemas: &HashMap<String, Arc<Valid<apollo_compiler::Schema>>>) {
        if let Some(node) = self.node.as_mut() {
            node.hash_subqueries(schemas);
        }
    }

    fn extract_authorization_metadata(
        &mut self,
        schema: &apollo_compiler::Schema,
        key: &CacheKeyMetadata,
    ) {
        if let Some(node) = self.node.as_mut() {
            node.extract_authorization_metadata(schema, key);
        }
    }
}

fn standardize_schema(mut schema: apollo_compiler::Schema) -> apollo_compiler::Schema {
    use apollo_compiler::schema::ExtendedType;

    fn standardize_value_for_comparison(value: &mut apollo_compiler::ast::Value) {
        use apollo_compiler::ast::Value;
        match value {
            Value::Object(object) => {
                for (_name, value) in object.iter_mut() {
                    standardize_value_for_comparison(value.make_mut());
                }
                object.sort_by_key(|(name, _value)| name.clone());
            }
            Value::List(list) => {
                for value in list {
                    standardize_value_for_comparison(value.make_mut());
                }
            }
            _ => {}
        }
    }

    fn standardize_directive_for_comparison(directive: &mut apollo_compiler::ast::Directive) {
        for arg in &mut directive.arguments {
            standardize_value_for_comparison(arg.make_mut().value.make_mut());
        }
        directive
            .arguments
            .sort_by_cached_key(|arg| arg.name.to_ascii_lowercase());
    }

    for ty in schema.types.values_mut() {
        match ty {
            ExtendedType::Object(object) => {
                let object = object.make_mut();
                object.fields.sort_keys();
                for field in object.fields.values_mut() {
                    let field = field.make_mut();
                    for arg in &mut field.arguments {
                        let arg = arg.make_mut();
                        if let Some(value) = &mut arg.default_value {
                            standardize_value_for_comparison(value.make_mut());
                        }
                        for directive in &mut arg.directives {
                            standardize_directive_for_comparison(directive.make_mut());
                        }
                    }
                    field
                        .arguments
                        .sort_by_cached_key(|arg| arg.name.to_ascii_lowercase());
                    for directive in &mut field.directives {
                        standardize_directive_for_comparison(directive.make_mut());
                    }
                }
                for directive in &mut object.directives.0 {
                    standardize_directive_for_comparison(directive.make_mut());
                }
            }
            ExtendedType::Interface(interface) => {
                let interface = interface.make_mut();
                interface.fields.sort_keys();
                for field in interface.fields.values_mut() {
                    let field = field.make_mut();
                    for arg in &mut field.arguments {
                        let arg = arg.make_mut();
                        if let Some(value) = &mut arg.default_value {
                            standardize_value_for_comparison(value.make_mut());
                        }
                        for directive in &mut arg.directives {
                            standardize_directive_for_comparison(directive.make_mut());
                        }
                    }
                    field
                        .arguments
                        .sort_by_cached_key(|arg| arg.name.to_ascii_lowercase());
                    for directive in &mut field.directives {
                        standardize_directive_for_comparison(directive.make_mut());
                    }
                }
                for directive in &mut interface.directives.0 {
                    standardize_directive_for_comparison(directive.make_mut());
                }
            }
            ExtendedType::InputObject(input_object) => {
                let input_object = input_object.make_mut();
                input_object.fields.sort_keys();
                for field in input_object.fields.values_mut() {
                    let field = field.make_mut();
                    if let Some(value) = &mut field.default_value {
                        standardize_value_for_comparison(value.make_mut());
                    }
                    for directive in &mut field.directives {
                        standardize_directive_for_comparison(directive.make_mut());
                    }
                }
                for directive in &mut input_object.directives {
                    standardize_directive_for_comparison(directive.make_mut());
                }
            }
            ExtendedType::Enum(enum_) => {
                let enum_ = enum_.make_mut();
                enum_.values.sort_keys();
                for directive in &mut enum_.directives {
                    standardize_directive_for_comparison(directive.make_mut());
                }
            }
            ExtendedType::Union(union_) => {
                let union_ = union_.make_mut();
                for directive in &mut union_.directives {
                    standardize_directive_for_comparison(directive.make_mut());
                }
            }
            ExtendedType::Scalar(scalar) => {
                let scalar = scalar.make_mut();
                for directive in &mut scalar.directives {
                    standardize_directive_for_comparison(directive.make_mut());
                }
            }
        }
    }

    schema
        .directive_definitions
        .sort_by_cached_key(|key, _value| key.to_ascii_lowercase());
    schema
        .types
        .sort_by_cached_key(|key, _value| key.to_ascii_lowercase());

    schema
}

fn render_diff(differences: &[diff::Result<&str>]) -> String {
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::json;
    use test_log::test;

    use super::*;
    use crate::json_ext::Path;
    use crate::metrics::FutureMetricsExt as _;
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
            let _planner = BridgeQueryPlanner::new(
                include_str!("../testdata/minimal_supergraph.graphql").into(),
                Default::default(),
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
            let _planner = BridgeQueryPlanner::new(
                include_str!("../testdata/minimal_fed2_supergraph.graphql").into(),
                Default::default(),
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
        let schema = Schema::parse(EXAMPLE_SCHEMA).unwrap();
        let query = include_str!("testdata/unknown_introspection_query.graphql");

        let planner = BridgeQueryPlanner::new(EXAMPLE_SCHEMA.to_string(), Default::default())
            .await
            .unwrap();

        let doc = Query::parse_document(query, None, &schema, &Configuration::default()).unwrap();

        let selections = planner
            .parse_selections(query.to_string(), None, &doc)
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
        macro_rules! s {
            ($query: expr) => {
                insta::assert_snapshot!(subselections_keys($query).await);
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

    async fn subselections_keys(query: &str) -> String {
        fn check_query_plan_coverage(
            node: &PlanNode,
            path: &Path,
            parent_label: Option<String>,
            subselections: &HashMap<SubSelectionKey, SubSelectionValue>,
        ) {
            match node {
                PlanNode::Defer { primary, deferred } => {
                    if let Some(subselection) = primary.subselection.clone() {
                        let path = path.join(primary.path.clone().unwrap_or_default());
                        assert!(
                            subselections.keys().any(|k| k.defer_label == parent_label),
                            "Missing key: '{}' '{:?}' '{}' in {:?}",
                            path,
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
                                    .any(|k| k.defer_label == deferred.label),
                                "Missing key: '{}' '{:?}' '{}'",
                                path,
                                deferred.label,
                                subselection
                            );
                        }
                        if let Some(node) = &deferred.node {
                            check_query_plan_coverage(
                                node,
                                &deferred.query_path,
                                deferred.label.clone(),
                                subselections,
                            )
                        }
                    }
                }
                PlanNode::Sequence { nodes } | PlanNode::Parallel { nodes } => {
                    for node in nodes {
                        check_query_plan_coverage(node, path, parent_label.clone(), subselections)
                    }
                }
                PlanNode::Fetch(_) => {}
                PlanNode::Flatten(flatten) => {
                    check_query_plan_coverage(&flatten.node, path, parent_label, subselections)
                }
                PlanNode::Condition {
                    condition: _,
                    if_clause,
                    else_clause,
                } => {
                    if let Some(node) = if_clause {
                        check_query_plan_coverage(node, path, parent_label.clone(), subselections)
                    }
                    if let Some(node) = else_clause {
                        check_query_plan_coverage(node, path, parent_label, subselections)
                    }
                }
                PlanNode::Subscription { rest, .. } => {
                    if let Some(node) = rest {
                        check_query_plan_coverage(node, path, parent_label, subselections)
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

        let result = plan(EXAMPLE_SCHEMA, query, query, None, PlanOptions::default())
            .await
            .unwrap();
        if let QueryPlannerContent::Plan { plan, .. } = result {
            check_query_plan_coverage(&plan.root, &Path::empty(), None, &plan.query.subselections);

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

        let planner = BridgeQueryPlanner::new(schema.to_string(), configuration.clone())
            .await
            .unwrap();

        let doc = Query::parse_document(
            original_query,
            operation_name.as_deref(),
            planner.schema().api_schema(),
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

    #[ignore]
    #[test]
    fn router_bridge_dependency_is_pinned() {
        let cargo_manifest: serde_json::Value = basic_toml::from_str(
            &fs::read_to_string(
                PathBuf::from(&env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .unwrap()
                    .join("Cargo.toml"),
            )
            .expect("could not read Cargo.toml"),
        )
        .expect("could not parse Cargo.toml");
        let router_bridge_version = cargo_manifest
            .get("workspace")
            .expect("Cargo.toml does not contain workspace")
            .as_object()
            .expect("Cargo.toml workspace key is not an object")
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
}
