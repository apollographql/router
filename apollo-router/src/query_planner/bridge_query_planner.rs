//! Calls out to nodejs query planner

use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Debug;
use std::fmt::Write;
use std::sync::Arc;
use std::time::Instant;

use apollo_compiler::ast;
use apollo_compiler::ast::Name;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::validation::Valid;
use apollo_compiler::Node;
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
use router_bridge::planner::ReferencedFieldsForType;
use router_bridge::planner::UsageReporting;
use serde::Deserialize;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use tower::Service;

use super::PlanNode;
use super::QueryKey;
use crate::configuration::ApolloMetricsGenerationMode;
use crate::configuration::GraphQLValidationMode;
use crate::error::PlanErrors;
use crate::error::QueryPlannerError;
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
use crate::query_planner::labeler::add_defer_labels;
use crate::services::layers::query_analysis::ParsedDocument;
use crate::services::layers::query_analysis::ParsedDocumentInner;
use crate::services::OperationKind;
use crate::services::QueryPlannerContent;
use crate::services::QueryPlannerRequest;
use crate::services::QueryPlannerResponse;
use crate::spec::Query;
use crate::spec::Schema;
use crate::spec::SpecError;
use crate::Configuration;

// For reporting validation results with `experimental_graphql_validation_mode: both`.
const VALIDATION_SOURCE_SCHEMA: &str = "schema";
const VALIDATION_SOURCE_OPERATION: &str = "operation";
const VALIDATION_FALSE_NEGATIVE: &str = "false_negative";
const VALIDATION_FALSE_POSITIVE: &str = "false_positive";
const VALIDATION_MATCH: &str = "match";

#[derive(Clone)]
/// A query planner that calls out to the nodejs router-bridge query planner.
///
/// No caching is performed. To cache, wrap in a [`CachingQueryPlanner`].
pub(crate) struct BridgeQueryPlanner {
    planner: Arc<Planner<QueryPlanResult>>,
    schema: Arc<Schema>,
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
        let schema = Schema::parse(&sdl, &configuration)?;

        let planner = Planner::new(
            sdl,
            QueryPlannerConfig {
                reuse_query_fragments: configuration.supergraph.reuse_query_fragments,
                incremental_delivery: Some(IncrementalDeliverySupport {
                    enable_defer: Some(configuration.supergraph.defer_support),
                }),
                graphql_validation: matches!(
                    configuration.experimental_graphql_validation_mode,
                    GraphQLValidationMode::Legacy | GraphQLValidationMode::Both
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
            },
        )
        .await;

        let planner = match planner {
            Ok(planner) => planner,
            Err(err) => {
                if configuration.experimental_graphql_validation_mode == GraphQLValidationMode::Both
                {
                    let has_validation_errors = err.iter().any(|err| err.is_validation_error());

                    if has_validation_errors && !schema.has_errors() {
                        tracing::warn!(
                            monotonic_counter.apollo.router.operations.validation = 1u64,
                            validation.source = VALIDATION_SOURCE_SCHEMA,
                            validation.result = VALIDATION_FALSE_NEGATIVE,
                            "validation mismatch: JS query planner reported a schema validation error, but apollo-rs did not"
                        );
                    }
                }

                return Err(err.into());
            }
        };

        if configuration.experimental_graphql_validation_mode == GraphQLValidationMode::Both {
            if schema.has_errors() {
                tracing::warn!(
                    monotonic_counter.apollo.router.operations.validation = 1u64,
                    validation.source = VALIDATION_SOURCE_SCHEMA,
                    validation.result = VALIDATION_FALSE_POSITIVE,
                    "validation mismatch: apollo-rs reported a schema validation error, but JS query planner did not"
                );
            } else {
                // false_negative was an early return so we know it was correct here
                tracing::info!(
                    monotonic_counter.apollo.router.operations.validation = 1u64,
                    validation.source = VALIDATION_SOURCE_SCHEMA,
                    validation.result = VALIDATION_MATCH
                );
            }
        }

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
        let api_schema = Schema::parse(&api_schema_string, &configuration)?;

        let schema = Arc::new(schema.with_api_schema(api_schema));
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
                        graphql_validation: matches!(
                            configuration.experimental_graphql_validation_mode,
                            GraphQLValidationMode::Legacy | GraphQLValidationMode::Both
                        ),
                        reuse_query_fragments: configuration.supergraph.reuse_query_fragments,
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
                    },
                )
                .await?,
        );

        let api_schema = planner.api_schema().await?;
        let api_schema = Schema::parse(&api_schema.schema, &configuration)?;
        let schema = Arc::new(Schema::parse(&schema, &configuration)?.with_api_schema(api_schema));

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

    async fn parse_selections(
        &self,
        query: String,
        operation_name: Option<&str>,
        doc: &ParsedDocument,
    ) -> Result<Query, QueryPlannerError> {
        Query::check_errors(doc)?;
        let executable = &doc.executable;
        crate::spec::operation_limits::check(
            &self.configuration,
            &query,
            executable,
            operation_name,
        )?;
        let validation_error = match self.configuration.experimental_graphql_validation_mode {
            GraphQLValidationMode::Legacy => None,
            GraphQLValidationMode::New => {
                Query::validate_query(doc)?;
                None
            }
            GraphQLValidationMode::Both => Query::validate_query(doc).err(),
        };

        let (fragments, operations, defer_stats, schema_aware_hash) =
            Query::extract_query_information(&self.schema, executable, &doc.ast)?;

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
            validation_error,
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
        fn is_validation_error(errors: &PlanErrors) -> bool {
            errors.errors.iter().all(|err| err.validation_error)
        }

        /// Compare errors from graphql-js and apollo-rs validation, and produce metrics on
        /// whether they had the same result.
        ///
        /// The result isn't inspected deeply: it only checks validation success/failure.
        fn compare_validation_errors(
            js_validation_error: Option<&PlanErrors>,
            rs_validation_error: Option<&crate::error::ValidationErrors>,
        ) {
            match (
                js_validation_error.map_or(false, is_validation_error),
                rs_validation_error,
            ) {
                (false, Some(validation_error)) => {
                    tracing::warn!(
                        monotonic_counter.apollo.router.operations.validation = 1u64,
                        validation.source = VALIDATION_SOURCE_OPERATION,
                        validation.result = VALIDATION_FALSE_POSITIVE,
                        "validation mismatch: JS query planner did not report query validation error, but apollo-rs did"
                    );
                    tracing::warn!(
                        "validation mismatch: Rust validation reported: {validation_error}"
                    );
                }
                (true, None) => {
                    tracing::warn!(
                        monotonic_counter.apollo.router.operations.validation = 1u64,
                        validation.source = VALIDATION_SOURCE_OPERATION,
                        validation.result = VALIDATION_FALSE_NEGATIVE,
                        "validation mismatch: apollo-rs did not report query validation error, but JS query planner did"
                    );
                    tracing::warn!(
                        "validation mismatch: JS validation reported: {}",
                        // Unwrapping is safe because `is_validation_error` is true
                        js_validation_error.unwrap(),
                    );
                }
                // if JS and Rust implementations agree, we return the JS result for now.
                _ => tracing::info!(
                    monotonic_counter.apollo.router.operations.validation = 1u64,
                    validation.source = VALIDATION_SOURCE_OPERATION,
                    validation.result = VALIDATION_MATCH,
                ),
            }
        }

        let planner_result = match self
            .planner
            .plan(filtered_query.clone(), operation.clone(), plan_options)
            .await
            .map_err(QueryPlannerError::RouterBridgeError)?
            .into_result()
        {
            Ok(mut plan) => {
                plan.data
                    .query_plan
                    .hash_subqueries(&self.schema.definitions);
                plan.data
                    .query_plan
                    .extract_authorization_metadata(&self.schema.definitions, &key);
                plan
            }
            Err(err) => {
                let plan_errors: PlanErrors = err.into();
                if matches!(
                    self.configuration.experimental_graphql_validation_mode,
                    GraphQLValidationMode::Both
                ) {
                    compare_validation_errors(
                        Some(&plan_errors),
                        selections.validation_error.as_ref(),
                    );
                }
                return Err(QueryPlannerError::from(plan_errors));
            }
        };

        if matches!(
            self.configuration.experimental_graphql_validation_mode,
            GraphQLValidationMode::Both
        ) {
            compare_validation_errors(None, selections.validation_error.as_ref());
        }

        // the `statsReportKey` field should match the original query instead of the filtered query, to index them all under the same query
        let operation_signature = if matches!(
            self.configuration.experimental_apollo_metrics_generation_mode,
            ApolloMetricsGenerationMode::Legacy | ApolloMetricsGenerationMode::Both
        ) && original_query != filtered_query {
            Some(
                self.planner
                    .operation_signature(original_query, operation.clone())
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
                    self.configuration.experimental_apollo_metrics_generation_mode,
                    ApolloMetricsGenerationMode::New | ApolloMetricsGenerationMode::Both
                ) {
                    // todo - are we generating the sig and refs from the original query and comparing it to the sig and refs
                    // generated from the filtered query in JS code? Is that ok? Do we need to re-parse the filtered query here?
                    
                    let generated_usage_reporting = UsageReporting {
                        stats_report_key: BridgeQueryPlanner::generate_apollo_reporting_signature(doc, operation.clone()),
                        referenced_fields_by_type: BridgeQueryPlanner::generate_apollo_reporting_refs(doc, operation, &self.schema.definitions),
                    };

                    if matches!(
                        self.configuration.experimental_apollo_metrics_generation_mode,
                        ApolloMetricsGenerationMode::Both
                    ) {
                        if usage_reporting.stats_report_key != generated_usage_reporting.stats_report_key {
                            println!(
                                "stats_report_key's are different:\n{}\n{}",  
                                generated_usage_reporting.stats_report_key,
                                usage_reporting.stats_report_key,
                            ); // todo remove
                            tracing::warn!(
                                monotonic_counter.apollo.router.operations.telemetry.studio.signature = 1u64,
                                generation.is_matched = false,
                                "Mismatch between the Apollo usage reporting signature generated in router and router-bridge"
                            );
                            tracing::debug!(
                                "Different signatures generated between router and router-bridge:\n{}\n{}",
                                generated_usage_reporting.stats_report_key,
                                usage_reporting.stats_report_key,
                            );
                        } else {
                            println!("stats_report_key's are identical"); // todo remove
                            tracing::info!(
                                monotonic_counter.apollo.router.operations.telemetry.studio.signature = 1u64,
                                generation.is_matched = true,
                            );
                        }

                        if usage_reporting.referenced_fields_by_type != generated_usage_reporting.referenced_fields_by_type {
                            println!(
                                "referenced_fields_by_type's are different:\n{:?}\n{:?}",  
                                generated_usage_reporting.referenced_fields_by_type,
                                usage_reporting.referenced_fields_by_type,
                            ); // todo remove
                            tracing::warn!(
                                monotonic_counter.apollo.router.operations.telemetry.studio.references = 1u64,
                                generation.is_matched = false,
                                "Mismatch between the Apollo usage report referenced fields generated in router and router-bridge"
                            );
                            tracing::debug!(
                                "Different referenced fields generated between router and router-bridge:\n{:?}\n{:?}",
                                generated_usage_reporting.referenced_fields_by_type,
                                usage_reporting.referenced_fields_by_type,
                            );
                        } else {
                            println!("referenced_fields_by_type's are identical"); // todo remove
                            tracing::info!(
                                monotonic_counter.apollo.router.operations.telemetry.studio.references = 1u64,
                                generation.is_matched = true,
                            );
                        }
                    } else if matches!(
                        self.configuration.experimental_apollo_metrics_generation_mode,
                        ApolloMetricsGenerationMode::New
                    ) {
                        usage_reporting.stats_report_key = generated_usage_reporting.stats_report_key;
                        usage_reporting.referenced_fields_by_type = generated_usage_reporting.referenced_fields_by_type;
                    }
                }

                Ok(QueryPlannerContent::Plan {
                    plan: Arc::new(super::QueryPlan {
                        usage_reporting,
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

    // todo move this somewhere else? somewhere where it's possible to do fuzzing?
    fn generate_apollo_reporting_signature(doc: &ParsedDocument, operation_name: Option<String>) -> String {
        // todo this implementation is temporary and needs a lot of work
        
        let definitions = doc.ast.definitions.clone();
        let operation_def = definitions[0].as_operation_definition().unwrap().clone();

        let operation_body = operation_def.to_string();
        // let operation_name = operation_def.name.as_ref().unwrap().to_string();
        // println!("operation_body: {}", operation_body);
        // println!("operation_name: {}", operation_name);

        let whitespace_regex = regex::Regex::new(r"\s+").unwrap();
        let stripped_body = whitespace_regex.replace_all(&operation_body, " ").to_string();

        format!("# {}\n{}", operation_name.unwrap_or_default(), stripped_body)
    }

    // todo move this somewhere else? somewhere where it's possible to do fuzzing?
    // todo probably needs a refactor (after testing/fixing)
    fn generate_apollo_reporting_refs(doc: &ParsedDocument, operation_name: Option<String>, schema: &Valid<apollo_compiler::Schema>) -> HashMap<String, ReferencedFieldsForType> {
        let maybe_op = doc.executable.get_operation(operation_name.as_deref()).ok();

        if let Some(operation) = maybe_op {
            let mut ref_fields = HashMap::new();
            let mut fragment_names: HashSet<Name> = HashSet::new();

            fn extract_fields(parent_type: String, selection_set: &SelectionSet, schema: &Valid<apollo_compiler::Schema>, ref_fields: &mut HashMap<String, ReferencedFieldsForType>, fragments: &mut HashSet<Name>) {
                // Extract child fields
                let child_fields: Vec<&Node<Field>> = selection_set.selections
                    .iter()
                    .filter_map(|selection| selection.as_field())
                    .collect();

                if !child_fields.is_empty() {
                    let field_names: Vec<String> = child_fields.iter().map(|field| field.name.to_string()).collect();
                    let field_schema_type = schema.types.get(parent_type.as_str());
                    let is_interface = field_schema_type.is_some_and(|t| t.is_interface());

                    let fields_for_type = ref_fields.entry(parent_type).or_insert(ReferencedFieldsForType {
                        field_names: vec![],
                        is_interface: is_interface,
                    });
                    // todo sort? use a temporary hashset instead?
                    for name in field_names {
                        if !fields_for_type.field_names.contains(&name) {
                            fields_for_type.field_names.push(name);
                        }
                    }

                    child_fields.iter().for_each(|field| {
                        let field_type = field.selection_set.ty.to_string();
                        extract_fields(field_type, &field.selection_set, schema, ref_fields, fragments);
                    });
                }

                // Extract fields from inline fragments
                selection_set.selections
                    .iter()
                    .filter_map(|selection| selection.as_inline_fragment())
                    .for_each(|frag| {
                        if let Some(fragment_type) = frag.type_condition.clone() {
                            let frag_type_name = fragment_type.to_string();
                            extract_fields(frag_type_name, &frag.selection_set, schema, ref_fields, fragments);
                        } else {
                            // todo not sure what to do for anonymous fragments - use parent_type?
                        }
                    });

                // Extract names of any used fragment spreads (these will be extracted later)
                selection_set.selections
                    .iter()
                    .filter_map(|selection| selection.as_fragment_spread())
                    .for_each(|frag| {
                        fragments.insert(frag.fragment_name.clone());
                    });
            }

            let operation_kind = OperationKind::from(operation.operation_type);
            extract_fields(operation_kind.as_str().into(), &operation.selection_set, schema, &mut ref_fields, &mut fragment_names);
    
            // Extract field from referenced fragment spreads
            // todo handle nested named fragments - need to keep track of seen fragments, pop fragments off the set when processed and push new ones on if they haven't already been seen
            fragment_names.iter().for_each(|name| {
                if let Some(fragment) = doc.executable.fragments.get(name) {
                    let mut temp_set: HashSet<Name> = HashSet::new();
                    let fragment_type = fragment.selection_set.ty.to_string();
                    extract_fields(fragment_type, &fragment.selection_set, schema, &mut ref_fields, &mut temp_set);
                }
            });

            ref_fields
        } else {
            // todo not sure what to do if operation can't be found - maybe empty map is ok?
            HashMap::new()
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
            let start = Instant::now();

            let mut doc = match context.extensions().lock().get::<ParsedDocument>() {
                None => return Err(QueryPlannerError::SpecError(SpecError::UnknownFileId)),
                Some(d) => d.clone(),
            };

            let schema = &this.schema.api_schema().definitions;
            match add_defer_labels(schema, &doc.ast) {
                Err(e) => {
                    return Err(QueryPlannerError::SpecError(SpecError::ParsingError(
                        e.to_string(),
                    )))
                }
                Ok(modified_query) => {
                    let executable_document = modified_query
                        .to_executable(schema)
                        // Assume transformation creates a valid document: ignore conversion errors
                        .unwrap_or_else(|invalid| invalid.partial);
                    doc = Arc::new(ParsedDocumentInner {
                        executable: Arc::new(executable_document),
                        ast: modified_query,
                        // Carry errors from previous ParsedDocument
                        // and assume transformation doesn’t introduce new errors.
                        // TODO: check the latter?
                        parse_errors: doc.parse_errors.clone(),
                        validation_errors: doc.validation_errors.clone(),
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
            let duration = start.elapsed().as_secs_f64();
            tracing::info!(histogram.apollo_router_query_planning_time = duration);

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
                                .insert(pe.usage_reporting.clone());
                        }
                        QueryPlannerError::SpecError(e) => {
                            context.extensions().lock().insert(UsageReporting {
                                stats_report_key: e.get_error_key().to_string(),
                                referenced_fields_by_type: HashMap::new(),
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
                .to_executable(&self.schema.api_schema().definitions)
                // Assume transformation creates a valid document: ignore conversion errors
                .unwrap_or_else(|invalid| invalid.partial);
            doc = Arc::new(ParsedDocumentInner {
                executable: Arc::new(executable_document),
                ast: new_doc,
                // Carry errors from previous ParsedDocument
                // and assume transformation doesn’t introduce new errors.
                // TODO: check the latter?
                parse_errors: doc.parse_errors.clone(),
                validation_errors: doc.validation_errors.clone(),
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
    fn hash_subqueries(&mut self, schema: &apollo_compiler::Schema) {
        if let Some(node) = self.node.as_mut() {
            node.hash_subqueries(schema);
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

    #[test(tokio::test)]
    async fn test_plan_invalid_query() {
        let err = plan(
            EXAMPLE_SCHEMA,
            "fragment UnusedTestFragment on User { id } query { me { id } }",
            "fragment UnusedTestFragment on User { id } query { me { id } }",
            None,
            PlanOptions::default(),
        )
        .await
        .unwrap_err();

        match err {
            QueryPlannerError::PlanningErrors(errors) => {
                insta::assert_debug_snapshot!("plan_invalid_query_errors", errors);
            }
            e => {
                panic!("invalid query planning should have failed: {e:?}");
            }
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
        let schema = Schema::parse(EXAMPLE_SCHEMA, &Default::default()).unwrap();
        let query = include_str!("testdata/unknown_introspection_query.graphql");

        let planner = BridgeQueryPlanner::new(EXAMPLE_SCHEMA.to_string(), Default::default())
            .await
            .unwrap();

        let doc = Query::parse_document(query, &schema, &Configuration::default());

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
        configuration.experimental_graphql_validation_mode = GraphQLValidationMode::Both;
        let configuration = Arc::new(configuration);

        let planner = BridgeQueryPlanner::new(schema.to_string(), configuration.clone())
            .await
            .unwrap();

        let doc = Query::parse_document(
            original_query,
            planner.schema().api_schema(),
            &configuration,
        );

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

    // todo more/better tests
    #[test(tokio::test)]
    async fn test_sig_and_ref_generation_1() {
        let configuration = Arc::new(Default::default());

        let schema_str = r#"type BasicTypesResponse {
            nullableId: ID
            nonNullId: ID!
            nullableInt: Int
            nonNullInt: Int!
            nullableString: String
            nonNullString: String!
            nullableFloat: Float
            nonNullFloat: Float!
            nullableBoolean: Boolean
            nonNullBoolean: Boolean!
          }
          
          enum SomeEnum {
            SOME_VALUE_1
            SOME_VALUE_2
            SOME_VALUE_3
          }
          
          interface AnInterface {
            sharedField: String!
          }
          
          type InterfaceImplementation1 implements AnInterface {
            sharedField: String!
            implementation1Field: Int!
          }
          
          type InterfaceImplementation2 implements AnInterface {
            sharedField: String!
            implementation2Field: Float!
          }
          
          type UnionType1 {
            unionType1Field: String!
            nullableString: String
          }
          
          type UnionType2 {
            unionType2Field: String!
            nullableString: String
          }
          
          union UnionType = UnionType1 | UnionType2
          
          type ObjectTypeResponse {
            stringField: String!
            intField: Int!
            nullableField: String
          }
          
          input NestedInputType {
            someFloat: Float!
            someNullableFloat: Float
          }
          
          input InputType {
            inputString: String!
            inputInt: Int!
            inputBoolean: Boolean
            nestedType: NestedInputType!
            enumInput: SomeEnum
            listInput: [Int!]!
            nestedTypeList: [NestedInputType]
          }
          
          input NestedEnumInputType {
            someEnum: SomeEnum
          }
          
          input AnotherInputType {
            anotherInput: ID!
          }
          
          input InputTypeWithDefault {
            nonNullId: ID!
            nonNullIdWithDefault: ID! = "id"
            nullableId: ID
            nullableIdWithDefault: ID = "id"
          }
          
          input EnumInputType {
            enumInput: SomeEnum!
            enumListInput: [SomeEnum!]!
            nestedEnumType: [NestedEnumInputType]
          }
          
          type EverythingResponse {
            basicTypes: BasicTypesResponse
            enumResponse: SomeEnum
            interfaceResponse: AnInterface
            interfaceImplementationResponse: InterfaceImplementation2
            unionResponse: UnionType
            unionType2Response: UnionType2
            listOfBools: [Boolean!]!
            listOfInterfaces: [AnInterface]
            listOfUnions: [UnionType]
            objectTypeWithInputField(boolInput: Boolean): ObjectTypeResponse
            listOfObjects: [ObjectTypeResponse]
          }
          
          type BasicResponse {
            id: Int!
            nullableId: Int
          }
          
          type Query {
            inputTypeQuery(input: InputType!): EverythingResponse!
            scalarInputQuery(
              listInput: [String!]!, 
              stringInput: String!, 
              nullableStringInput: String, 
              intInput: Int!, 
              floatInput: Float!, 
              boolInput: Boolean!, 
              enumInput: SomeEnum,
              idInput: ID!
            ): EverythingResponse!
            noInputQuery: EverythingResponse!
            basicInputTypeQuery(input: NestedInputType!): EverythingResponse!
            anotherInputTypeQuery(input: AnotherInputType): EverythingResponse!
            enumInputQuery(enumInput: SomeEnum, inputType: EnumInputType): EverythingResponse!
            basicResponseQuery: BasicResponse!
            scalarResponseQuery: String
            defaultArgQuery(stringInput: String! = "default", inputType: AnotherInputType = { anotherInput: "inputDefault" }): BasicResponse!
            inputTypehDefaultQuery(input: InputTypeWithDefault): BasicResponse!
          }"#;
        let schema = Schema::parse(schema_str, &Default::default()).unwrap();

        let query = r#"query UnusedQuery {
            noInputQuery {
              enumResponse
            }
          }
          
          fragment UnusedFragment on EverythingResponse {
            enumResponse
          }
          
          fragment Fragment2 on EverythingResponse {
            basicTypes {
              nullableFloat
            }
          }
          
          query        TransformedQuery    {
          
          
            scalarInputQuery(idInput: "a1", listInput: [], boolInput: true, intInput: 1, stringInput: "x", floatInput: 1.2)      @skip(if: false)   @include(if: true) {
              ...Fragment2,
          
          
              objectTypeWithInputField(boolInput: true, secondInput: false) {
                stringField
                __typename
                intField
              }
          
              enumResponse
              interfaceResponse {
                sharedField
                ... on InterfaceImplementation2 {
                  implementation2Field
                }
                ... on InterfaceImplementation1 {
                  implementation1Field
                }
              }
              ...Fragment1,
            }
          }
          
          fragment Fragment1 on EverythingResponse {
            basicTypes {
              nonNullFloat
            }
          }"#;

        let doc = Query::parse_document(query, &schema, &configuration);

        //let generated_sig = BridgeQueryPlanner::generate_apollo_reporting_signature(&doc);
        //let expected_sig = "# TransformedQuery\nfragment Fragment1 on EverythingResponse{basicTypes{nonNullFloat}}fragment Fragment2 on EverythingResponse{basicTypes{nullableFloat}}query TransformedQuery{scalarInputQuery(boolInput:true floatInput:0 idInput:\"\"intInput:0 listInput:[]stringInput:\"\")@skip(if:false)@include(if:true){enumResponse interfaceResponse{sharedField...on InterfaceImplementation2{implementation2Field}...on InterfaceImplementation1{implementation1Field}}objectTypeWithInputField(boolInput:true,secondInput:false){__typename intField stringField}...Fragment1...Fragment2}}";
        //assert_eq!(expected_sig, generated_sig);

        let generated_refs = BridgeQueryPlanner::generate_apollo_reporting_refs(&doc, Some("TransformedQuery".into()), &schema.api_schema().definitions);
        let expected_refs = HashMap::from([
            ("Query".into(), ReferencedFieldsForType {
                field_names: vec!["scalarInputQuery".into()],
                is_interface: false,
            }),
            ("BasicTypesResponse".into(), ReferencedFieldsForType {
                field_names: vec!["nullableFloat".into(), "nonNullFloat".into()],
                is_interface: false,
            }),
            ("EverythingResponse".into(), ReferencedFieldsForType {
                field_names: vec![
                    "basicTypes".into(), 
                    "objectTypeWithInputField".into(),
                    "enumResponse".into(),
                    "interfaceResponse".into(),
                ],
                is_interface: false,
            }),
            ("AnInterface".into(), ReferencedFieldsForType {
                field_names: vec!["sharedField".into()],
                is_interface: true,
            }),
            ("ObjectTypeResponse".into(), ReferencedFieldsForType {
                field_names: vec!["stringField".into(), "__typename".into(), "intField".into()],
                is_interface: false,
            }),
            ("InterfaceImplementation1".into(), ReferencedFieldsForType {
                field_names: vec!["implementation1Field".into()],
                is_interface: false,
            }),
            ("InterfaceImplementation2".into(), ReferencedFieldsForType {
                field_names: vec!["implementation2Field".into()],
                is_interface: false,
            }),
        ]);
        assert_eq!(expected_refs, generated_refs);
    }

    #[test(tokio::test)]
    async fn test_sig_and_ref_generation_2() {
        let configuration = Arc::new(Default::default());

        let schema_str = r#"type BasicTypesResponse {
            nullableId: ID
            nonNullId: ID!
            nullableInt: Int
            nonNullInt: Int!
            nullableString: String
            nonNullString: String!
            nullableFloat: Float
            nonNullFloat: Float!
            nullableBoolean: Boolean
            nonNullBoolean: Boolean!
          }
          
          enum SomeEnum {
            SOME_VALUE_1
            SOME_VALUE_2
            SOME_VALUE_3
          }
          
          interface AnInterface {
            sharedField: String!
          }
          
          type InterfaceImplementation1 implements AnInterface {
            sharedField: String!
            implementation1Field: Int!
          }
          
          type InterfaceImplementation2 implements AnInterface {
            sharedField: String!
            implementation2Field: Float!
          }
          
          type UnionType1 {
            unionType1Field: String!
            nullableString: String
          }
          
          type UnionType2 {
            unionType2Field: String!
            nullableString: String
          }
          
          union UnionType = UnionType1 | UnionType2
          
          type ObjectTypeResponse {
            stringField: String!
            intField: Int!
            nullableField: String
          }
          
          input NestedInputType {
            someFloat: Float!
            someNullableFloat: Float
          }
          
          input InputType {
            inputString: String!
            inputInt: Int!
            inputBoolean: Boolean
            nestedType: NestedInputType!
            enumInput: SomeEnum
            listInput: [Int!]!
            nestedTypeList: [NestedInputType]
          }
          
          input NestedEnumInputType {
            someEnum: SomeEnum
          }
          
          input AnotherInputType {
            anotherInput: ID!
          }
          
          input InputTypeWithDefault {
            nonNullId: ID!
            nonNullIdWithDefault: ID! = "id"
            nullableId: ID
            nullableIdWithDefault: ID = "id"
          }
          
          input EnumInputType {
            enumInput: SomeEnum!
            enumListInput: [SomeEnum!]!
            nestedEnumType: [NestedEnumInputType]
          }
          
          type EverythingResponse {
            basicTypes: BasicTypesResponse
            enumResponse: SomeEnum
            interfaceResponse: AnInterface
            interfaceImplementationResponse: InterfaceImplementation2
            unionResponse: UnionType
            unionType2Response: UnionType2
            listOfBools: [Boolean!]!
            listOfInterfaces: [AnInterface]
            listOfUnions: [UnionType]
            objectTypeWithInputField(boolInput: Boolean): ObjectTypeResponse
            listOfObjects: [ObjectTypeResponse]
          }
          
          type BasicResponse {
            id: Int!
            nullableId: Int
          }
          
          type Query {
            inputTypeQuery(input: InputType!): EverythingResponse!
            scalarInputQuery(
              listInput: [String!]!, 
              stringInput: String!, 
              nullableStringInput: String, 
              intInput: Int!, 
              floatInput: Float!, 
              boolInput: Boolean!, 
              enumInput: SomeEnum,
              idInput: ID!
            ): EverythingResponse!
            noInputQuery: EverythingResponse!
            basicInputTypeQuery(input: NestedInputType!): EverythingResponse!
            anotherInputTypeQuery(input: AnotherInputType): EverythingResponse!
            enumInputQuery(enumInput: SomeEnum, inputType: EnumInputType): EverythingResponse!
            basicResponseQuery: BasicResponse!
            scalarResponseQuery: String
            defaultArgQuery(stringInput: String! = "default", inputType: AnotherInputType = { anotherInput: "inputDefault" }): BasicResponse!
            inputTypehDefaultQuery(input: InputTypeWithDefault): BasicResponse!
          }"#;
        let schema = Schema::parse(schema_str, &Default::default()).unwrap();

        let query = r#"query Query($secondInput: Boolean!) {
            scalarResponseQuery
            noInputQuery {
              basicTypes {
                nonNullId
                nonNullInt
              }
              enumResponse
              interfaceImplementationResponse {
                sharedField
                implementation2Field
              }
              interfaceResponse {
                ... on InterfaceImplementation1 {
                  implementation1Field
                  sharedField
                }
                ... on InterfaceImplementation2 {
                  implementation2Field
                  sharedField
                }
              }
              listOfUnions {
                ... on UnionType1 {
                  nullableString
                }
              }
              objectTypeWithInputField(secondInput: $secondInput) {
                intField
              }
            }
            basicInputTypeQuery(input: { someFloat: 1 }) {
              unionResponse {
                ... on UnionType1 {
                  nullableString
                }
              }
              unionType2Response {
                unionType2Field
              }
              listOfObjects {
                stringField
              }
            }
          }"#;

        let doc = Query::parse_document(query, &schema, &configuration);

        //let generated_sig = BridgeQueryPlanner::generate_apollo_reporting_signature(&doc);
        //let expected_sig = "# Query\nquery Query($secondInput:Boolean!){basicInputTypeQuery(input:{}){listOfObjects{stringField}unionResponse{...on UnionType1{nullableString}}unionType2Response{unionType2Field}}noInputQuery{basicTypes{nonNullId nonNullInt}enumResponse interfaceImplementationResponse{implementation2Field sharedField}interfaceResponse{...on InterfaceImplementation1{implementation1Field sharedField}...on InterfaceImplementation2{implementation2Field sharedField}}listOfUnions{...on UnionType1{nullableString}}objectTypeWithInputField(secondInput:$secondInput){intField}}scalarResponseQuery}";
        //assert_eq!(expected_sig, generated_sig);

        let generated_refs = BridgeQueryPlanner::generate_apollo_reporting_refs(&doc, Some("Query".into()), &schema.api_schema().definitions);
        let expected_refs = HashMap::from([
            ("Query".into(), ReferencedFieldsForType {
                field_names: vec!["scalarResponseQuery".into(), "noInputQuery".into(), "basicInputTypeQuery".into()],
                is_interface: false,
            }),
            ("BasicTypesResponse".into(), ReferencedFieldsForType {
                field_names: vec!["nonNullId".into(), "nonNullInt".into()],
                is_interface: false,
            }),
            ("ObjectTypeResponse".into(), ReferencedFieldsForType {
                field_names: vec!["intField".into(), "stringField".into()],
                is_interface: false,
            }),
            ("UnionType2".into(), ReferencedFieldsForType {
                field_names: vec!["unionType2Field".into()],
                is_interface: false,
            }),
            ("EverythingResponse".into(), ReferencedFieldsForType {
                field_names: vec![
                    "basicTypes".into(),
                    "enumResponse".into(),
                    "interfaceImplementationResponse".into(),
                    "interfaceResponse".into(),
                    "listOfUnions".into(),
                    "objectTypeWithInputField".into(),
                    "unionResponse".into(),
                    "unionType2Response".into(),
                    "listOfObjects".into(),
                ],
                is_interface: false,
            }),
            ("InterfaceImplementation1".into(), ReferencedFieldsForType {
                field_names: vec!["implementation1Field".into(), "sharedField".into()],
                is_interface: false,
            }),
            ("UnionType1".into(), ReferencedFieldsForType {
                field_names: vec!["nullableString".into()],
                is_interface: false,
            }),
            ("InterfaceImplementation2".into(), ReferencedFieldsForType {
                field_names: vec!["sharedField".into(), "implementation2Field".into()],
                is_interface: false,
            }),
        ]);
        assert_eq!(expected_refs, generated_refs);
    }
}
