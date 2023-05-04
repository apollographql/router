//! Calls out to nodejs query planner

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Instant;

use futures::future::BoxFuture;
use router_bridge::planner::IncrementalDeliverySupport;
use router_bridge::planner::PlanSuccess;
use router_bridge::planner::Planner;
use router_bridge::planner::QueryPlannerConfig;
use router_bridge::planner::UsageReporting;
use serde::Deserialize;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use tower::Service;
use tracing::Instrument;

use super::PlanNode;
use super::QueryKey;
use crate::error::QueryPlannerError;
use crate::error::ServiceBuildError;
use crate::graphql;
use crate::introspection::Introspection;
use crate::services::QueryPlannerContent;
use crate::services::QueryPlannerRequest;
use crate::services::QueryPlannerResponse;
use crate::spec::Query;
use crate::spec::Schema;
use crate::Configuration;

pub(crate) static USAGE_REPORTING: &str = "apollo_telemetry::usage_reporting";

#[derive(Clone)]
/// A query planner that calls out to the nodejs router-bridge query planner.
///
/// No caching is performed. To cache, wrap in a [`CachingQueryPlanner`].
pub(crate) struct BridgeQueryPlanner {
    planner: Arc<Planner<QueryPlanResult>>,
    schema: Arc<Schema>,
    introspection: Option<Arc<Introspection>>,
    configuration: Arc<Configuration>,
}

impl BridgeQueryPlanner {
    pub(crate) async fn new(
        schema: String,
        configuration: Arc<Configuration>,
    ) -> Result<Self, ServiceBuildError> {
        let planner = Arc::new(
            Planner::new(
                schema.clone(),
                QueryPlannerConfig {
                    incremental_delivery: Some(IncrementalDeliverySupport {
                        enable_defer: Some(configuration.supergraph.defer_support),
                    }),
                },
            )
            .await?,
        );

        let api_schema = planner.api_schema().await?;
        let api_schema = Schema::parse(&api_schema.schema, &configuration, None)?;
        let schema = Arc::new(Schema::parse(
            &schema,
            &configuration,
            Some(Box::new(api_schema)),
        )?);
        let introspection = if configuration.supergraph.introspection {
            Some(Arc::new(Introspection::new(planner.clone()).await))
        } else {
            None
        };
        Ok(Self {
            planner,
            schema,
            introspection,
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
                    },
                )
                .await?,
        );

        let api_schema = planner.api_schema().await?;
        let api_schema = Schema::parse(&api_schema.schema, &configuration, None)?;
        let schema = Arc::new(Schema::parse(
            &schema,
            &configuration,
            Some(Box::new(api_schema)),
        )?);

        let introspection = if configuration.supergraph.introspection {
            Some(Arc::new(Introspection::new(planner.clone()).await))
        } else {
            None
        };

        Ok(Self {
            planner,
            schema,
            introspection,
            configuration,
        })
    }

    pub(crate) fn planner(&self) -> Arc<Planner<QueryPlanResult>> {
        self.planner.clone()
    }

    pub(crate) fn schema(&self) -> Arc<Schema> {
        self.schema.clone()
    }

    async fn parse_selections(&self, key: QueryKey) -> Result<Query, QueryPlannerError> {
        let (query, operation_name) = key;
        let schema = self.schema.clone();
        let configuration = self.configuration.clone();
        let task_result = tokio::task::spawn_blocking(move || {
            let mut query = Query::parse(query, &schema, &configuration)?;
            crate::spec::operation_limits::check(&configuration, &mut query, operation_name)?;
            Ok::<_, QueryPlannerError>(query)
        })
        .instrument(tracing::info_span!("parse_query", "otel.kind" = "INTERNAL"))
        .await;
        if let Err(err) = &task_result {
            failfast_debug!("parsing query task failed: {}", err);
        }
        task_result?
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
        query: String,
        operation: Option<String>,
        mut selections: Query,
    ) -> Result<QueryPlannerContent, QueryPlannerError> {
        let planner_result = self
            .planner
            .plan(query, operation)
            .await
            .map_err(QueryPlannerError::RouterBridgeError)?
            .into_result()
            .map_err(QueryPlannerError::from)?;

        match planner_result {
            PlanSuccess {
                data:
                    QueryPlanResult {
                        query_plan: QueryPlan { node: Some(node) },
                        formatted_query_plan,
                    },
                usage_reporting,
            } => {
                let subselections = node.parse_subselections(&self.schema)?;
                selections.subselections = subselections;
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
        let this = self.clone();
        let fut = async move {
            let start = Instant::now();
            let res = this
                .get((req.query.clone(), req.operation_name.to_owned()))
                .await;
            let duration = start.elapsed().as_secs_f64();
            tracing::info!(histogram.apollo_router_query_planning_time = duration,);

            match res {
                Ok(query_planner_content) => Ok(QueryPlannerResponse::builder()
                    .content(query_planner_content)
                    .context(req.context)
                    .build()),
                Err(e) => {
                    match &e {
                        QueryPlannerError::PlanningErrors(pe) => {
                            if let Err(inner_e) = req
                                .context
                                .insert(USAGE_REPORTING, pe.usage_reporting.clone())
                            {
                                tracing::error!(
                                    "usage reporting was not serializable to context, {}",
                                    inner_e
                                );
                            }
                        }
                        QueryPlannerError::SpecError(e) => {
                            if let Err(inner_e) = req.context.insert(
                                USAGE_REPORTING,
                                UsageReporting {
                                    stats_report_key: e.get_error_key().to_string(),
                                    referenced_fields_by_type: HashMap::new(),
                                },
                            ) {
                                tracing::error!(
                                    "usage reporting was not serializable to context, {}",
                                    inner_e
                                );
                            }
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

impl BridgeQueryPlanner {
    async fn get(&self, key: QueryKey) -> Result<QueryPlannerContent, QueryPlannerError> {
        let selections = self.parse_selections(key.clone()).await?;

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
                return self.introspection(key.0).await;
            }
        }

        self.plan(key.0, key.1, selections).await
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
    use serde_json::json;
    use test_log::test;

    use super::*;

    const EXAMPLE_SCHEMA: &str = include_str!("testdata/schema.graphql");

    #[test(tokio::test)]
    async fn test_plan() {
        let planner = BridgeQueryPlanner::new(EXAMPLE_SCHEMA.to_string(), Default::default())
            .await
            .unwrap();
        let result = planner
            .get((include_str!("testdata/query.graphql").into(), None))
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
        let planner = BridgeQueryPlanner::new(EXAMPLE_SCHEMA.to_string(), Default::default())
            .await
            .unwrap();
        let err = planner
            .get((
                "fragment UnusedTestFragment on User { id } query { me { id } }".to_string(),
                None,
            ))
            .await
            .unwrap_err();

        match err {
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
        let err = BridgeQueryPlanner::new(EXAMPLE_SCHEMA.to_string(), Default::default())
            .await
            .unwrap()
            // test the planning part separately because it is a valid introspection query
            // it should be caught by the introspection part, but just in case, we check
            // that the query planner would return an empty plan error if it received an
            // introspection query
            .plan(
                include_str!("testdata/unknown_introspection_query.graphql").into(),
                None,
                Query::default(),
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
        let planner = BridgeQueryPlanner::new(EXAMPLE_SCHEMA.to_string(), Default::default())
            .await
            .unwrap();
        let result = planner.get(("".into(), None)).await;

        assert_eq!(
            "couldn't plan query: query validation errors: Syntax Error: Unexpected <EOF>.",
            result.unwrap_err().to_string()
        );
    }

    #[test(tokio::test)]
    async fn test_single_aliased_root_typename() {
        let planner = BridgeQueryPlanner::new(EXAMPLE_SCHEMA.to_string(), Default::default())
            .await
            .unwrap();
        let result = planner
            .get(("{ x: __typename }".into(), None))
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
        let planner = BridgeQueryPlanner::new(EXAMPLE_SCHEMA.to_string(), Default::default())
            .await
            .unwrap();
        let result = planner
            .get(("{ x: __typename __typename }".into(), None))
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
}
