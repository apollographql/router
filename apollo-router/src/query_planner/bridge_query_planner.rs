//! Calls out to nodejs query planner

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Instant;

use apollo_compiler::ApolloCompiler;
use apollo_compiler::InputDatabase;
use futures::future::BoxFuture;
use router_bridge::planner::IncrementalDeliverySupport;
use router_bridge::planner::PlanSuccess;
use router_bridge::planner::Planner;
use router_bridge::planner::QueryPlannerConfig;
use router_bridge::planner::UsageReporting;
use serde::Deserialize;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use tokio::sync::Mutex;
use tower::Service;

use super::PlanNode;
use super::QueryKey;
use crate::configuration::GraphQLValidationMode;
use crate::error::QueryPlannerError;
use crate::error::ServiceBuildError;
use crate::graphql;
use crate::introspection::Introspection;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::query_planner::labeler::add_defer_labels;
use crate::services::layers::query_analysis::Compiler;
use crate::services::QueryPlannerContent;
use crate::services::QueryPlannerRequest;
use crate::services::QueryPlannerResponse;
use crate::spec::query::QUERY_EXECUTABLE;
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
                            monotonic_counter.apollo.router.validation = 1u64,
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
                    monotonic_counter.apollo.router.validation = 1u64,
                    validation.source = VALIDATION_SOURCE_SCHEMA,
                    validation.result = VALIDATION_FALSE_POSITIVE,
                    "validation mismatch: apollo-rs reported a schema validation error, but JS query planner did not"
                );
            } else {
                // false_negative was an early return so we know it was correct here
                tracing::info!(
                    monotonic_counter.apollo.router.validation = 1u64,
                    validation.source = VALIDATION_SOURCE_SCHEMA,
                    validation.result = VALIDATION_MATCH
                );
            }
        }

        let planner = Arc::new(planner);

        let api_schema = planner.api_schema().await?;
        let api_schema = Schema::parse(&api_schema.schema, &configuration)?;
        let schema = Arc::new(schema.with_api_schema(api_schema));
        let introspection = if configuration.supergraph.introspection {
            Some(Arc::new(Introspection::new(planner.clone()).await))
        } else {
            None
        };

        let enable_authorization_directives =
            AuthorizationPlugin::enable_directives(&configuration, &schema)?;
        Ok(Self {
            planner,
            schema,
            introspection,
            enable_authorization_directives,
            configuration,
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
                    },
                )
                .await?,
        );

        let api_schema = planner.api_schema().await?;
        let api_schema = Schema::parse(&api_schema.schema, &configuration)?;
        let schema = Arc::new(Schema::parse(&schema, &configuration)?.with_api_schema(api_schema));

        let introspection = if configuration.supergraph.introspection {
            Some(Arc::new(Introspection::new(planner.clone()).await))
        } else {
            None
        };

        let enable_authorization_directives =
            AuthorizationPlugin::enable_directives(&configuration, &schema)?;
        Ok(Self {
            planner,
            schema,
            introspection,
            enable_authorization_directives,
            configuration,
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
        operation_name: Option<String>,
        compiler: Arc<Mutex<ApolloCompiler>>,
    ) -> Result<Query, QueryPlannerError> {
        let compiler_guard = compiler.lock().await;

        crate::spec::operation_limits::check(
            &self.configuration,
            &query,
            &compiler_guard,
            operation_name.clone(),
        )?;
        let file_id = compiler_guard
            .db
            .source_file(QUERY_EXECUTABLE.into())
            .ok_or_else(|| {
                QueryPlannerError::SpecError(SpecError::ValidationError(
                    "missing input file for query".to_string(),
                ))
            })?;

        Query::check_errors(&compiler_guard, file_id)?;
        let validation_error = match self.configuration.experimental_graphql_validation_mode {
            GraphQLValidationMode::Legacy => None,
            GraphQLValidationMode::New => {
                Query::validate_query(&compiler_guard, file_id)?;
                None
            }
            GraphQLValidationMode::Both => Query::validate_query(&compiler_guard, file_id).err(),
        };

        let (fragments, operations, defer_stats) =
            Query::extract_query_information(&compiler_guard, &self.schema)?;

        drop(compiler_guard);

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
            unauthorized_paths: vec![],
            subselections,
            defer_stats,
            is_original: true,
            validation_error,
        })
    }

    async fn introspection(&self, query: String) -> Result<QueryPlannerContent, QueryPlannerError> {
        match self.introspection.as_ref() {
            Some(introspection) => {
                let response = introspection
                    .execute(query)
                    .await
                    .map_err(QueryPlannerError::Introspection)?;

                Ok(QueryPlannerContent::Introspection {
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
        selections: Query,
    ) -> Result<QueryPlannerContent, QueryPlannerError> {
        /// Compare errors from graphql-js and apollo-rs validation, and produce metrics on
        /// whether they had the same result.
        ///
        /// The result isn't inspected deeply: it only checks validation success/failure.
        fn compare_validation_errors(
            js_validation_error: Option<&router_bridge::planner::PlanErrors>,
            rs_validation_error: Option<&crate::error::ValidationErrors>,
        ) {
            let is_validation_error = js_validation_error
                .map_or(false, |js| js.errors.iter().all(|err| err.validation_error));
            match (is_validation_error, rs_validation_error) {
                (false, Some(validation_error)) => {
                    tracing::warn!(
                        monotonic_counter.apollo.router.validation = 1u64,
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
                        monotonic_counter.apollo.router.validation = 1u64,
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
                    monotonic_counter.apollo.router.validation = 1u64,
                    validation.source = VALIDATION_SOURCE_OPERATION,
                    validation.result = VALIDATION_MATCH,
                ),
            }
        }

        let planner_result = self
            .planner
            .plan(filtered_query.clone(), operation.clone())
            .await
            .map_err(QueryPlannerError::RouterBridgeError)?
            .into_result()
            .map_err(|err| {
                if matches!(
                    self.configuration.experimental_graphql_validation_mode,
                    GraphQLValidationMode::Both
                ) {
                    compare_validation_errors(Some(&err), selections.validation_error.as_ref());
                }

                QueryPlannerError::from(err)
            })?;

        if matches!(
            self.configuration.experimental_graphql_validation_mode,
            GraphQLValidationMode::Both
        ) {
            compare_validation_errors(None, selections.validation_error.as_ref());
        }

        // the `statsReportKey` field should match the original query instead of the filtered query, to index them all under the same query
        let operation_signature = if original_query != filtered_query {
            Some(
                self.planner
                    .operation_signature(original_query, operation)
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
            .private_entries
            .lock()
            .get::<CacheKeyMetadata>()
            .cloned()
            .unwrap_or_default();
        let this = self.clone();
        let fut = async move {
            let start = Instant::now();

            let compiler = match context.private_entries.lock().get::<Compiler>() {
                None => {
                    return Err(QueryPlannerError::SpecError(SpecError::ParsingError(
                        "missing compiler".to_string(),
                    )))
                }
                Some(c) => c.0.clone(),
            };
            let mut compiler_guard = compiler.lock().await;
            let file_id = compiler_guard
                .db
                .source_file(QUERY_EXECUTABLE.into())
                .ok_or(QueryPlannerError::SpecError(SpecError::ParsingError(
                    "missing input file for query".to_string(),
                )))?;

            match add_defer_labels(file_id, &compiler_guard) {
                Err(e) => {
                    return Err(QueryPlannerError::SpecError(SpecError::ParsingError(
                        e.to_string(),
                    )))
                }
                Ok(modified_query) => {
                    // We’ve already checked the original query against the configured token limit
                    // when first parsing it.
                    // We’ve now serialized a modified query (with labels added) and are about
                    // to re-parse it, but that’s an internal detail that should not affect
                    // which original queries are rejected because of the token limit.
                    compiler_guard.db.set_token_limit(None);
                    compiler_guard.update_executable(file_id, &modified_query);
                }
            }

            let filtered_query = compiler_guard.db.source_code(file_id);
            drop(compiler_guard);

            let res = this
                .get(
                    QueryKey {
                        original_query,
                        filtered_query: filtered_query.to_string(),
                        operation_name: operation_name.to_owned(),
                        metadata,
                    },
                    compiler,
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
                                .private_entries
                                .lock()
                                .insert(pe.usage_reporting.clone());
                        }
                        QueryPlannerError::SpecError(e) => {
                            context.private_entries.lock().insert(UsageReporting {
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
pub(crate) type FilteredQuery = (String, Vec<Path>, Arc<Mutex<ApolloCompiler>>);

impl BridgeQueryPlanner {
    async fn get(
        &self,
        mut key: QueryKey,
        mut compiler: Arc<Mutex<ApolloCompiler>>,
    ) -> Result<QueryPlannerContent, QueryPlannerError> {
        let filter_res = if self.enable_authorization_directives {
            match AuthorizationPlugin::filter_query(&key, &self.schema) {
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
                    return Ok(QueryPlannerContent::Introspection {
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
                key.operation_name.clone(),
                compiler.clone(),
            )
            .await?;

        if let Some((filtered_query, unauthorized_paths, new_compiler)) = filter_res {
            key.filtered_query = filtered_query;
            compiler = new_compiler;
            selections.unauthorized_paths = unauthorized_paths;
        }

        if selections.contains_introspection() {
            // If we have only one operation containing only the root field `__typename`
            // (possibly aliased or repeated). (This does mean we fail to properly support
            // {"query": "query A {__typename} query B{somethingElse}", "operationName":"A"}.)
            if let Some(output_keys) = selections
                .operations
                .get(0)
                .and_then(|op| op.is_only_typenames_with_output_keys())
            {
                let operation_name = selections.operations[0].kind().to_string();
                let data: Value = Value::Object(Map::from_iter(
                    output_keys
                        .into_iter()
                        .map(|key| (key, Value::String(operation_name.clone().into()))),
                ));
                return Ok(QueryPlannerContent::Introspection {
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
                    key.operation_name.clone(),
                    compiler,
                )
                .await?;
            filtered.is_original = false;
            selections.filtered_query = Some(Arc::new(filtered));
        }

        self.plan(
            key.original_query,
            key.filtered_query,
            key.operation_name,
            selections,
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::json;
    use test_log::test;

    use super::*;
    use crate::json_ext::Path;
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
        )
        .await
        .unwrap_err();

        match err {
            // XXX(@goto-bus-stop): will be a SpecError in the Rust-based validation implementation
            QueryPlannerError::PlanningErrors(plan_errors) => {
                insta::with_settings!({sort_maps => true}, {
                    insta::assert_json_snapshot!("plan_invalid_query_usage_reporting", plan_errors.usage_reporting);
                });
                insta::assert_debug_snapshot!("plan_invalid_query_errors", plan_errors.errors);
            }
            _ => {
                panic!("invalid query planning should have failed");
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
    async fn empty_query_plan_should_be_a_planner_error() {
        let schema = Schema::parse(EXAMPLE_SCHEMA, &Default::default()).unwrap();
        let query = include_str!("testdata/unknown_introspection_query.graphql");

        let planner = BridgeQueryPlanner::new(EXAMPLE_SCHEMA.to_string(), Default::default())
            .await
            .unwrap();

        let compiler = Query::make_compiler(query, &schema, &Configuration::default()).0;

        let selections = planner
            .parse_selections(query.to_string(), None, Arc::new(Mutex::new(compiler)))
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
                selections,
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
        let result = plan(EXAMPLE_SCHEMA, "", "", None).await;

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
        )
        .await
        .unwrap();
        if let QueryPlannerContent::Introspection { response } = result {
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
        )
        .await
        .unwrap();
        if let QueryPlannerContent::Introspection { response } = result {
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

        dbg!(query);
        let result = plan(EXAMPLE_SCHEMA, query, query, None).await.unwrap();
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
    ) -> Result<QueryPlannerContent, QueryPlannerError> {
        let mut configuration: Configuration = Default::default();
        configuration.supergraph.introspection = true;
        configuration.experimental_graphql_validation_mode = GraphQLValidationMode::Both;
        let configuration = Arc::new(configuration);

        let planner = BridgeQueryPlanner::new(schema.to_string(), configuration.clone())
            .await
            .unwrap();

        let (compiler, _) = Query::make_compiler(
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
                },
                Arc::new(Mutex::new(compiler)),
            )
            .await
    }

    #[test]
    fn router_bridge_dependency_is_pinned() {
        let cargo_manifest: toml::Value =
            fs::read_to_string(PathBuf::from(&env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
                .expect("could not read Cargo.toml")
                .parse()
                .expect("could not parse Cargo.toml");
        let router_bridge_version = cargo_manifest
            .get("dependencies")
            .expect("Cargo.toml does not contain dependencies")
            .as_table()
            .expect("Cargo.toml dependencies key is not a table")
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
