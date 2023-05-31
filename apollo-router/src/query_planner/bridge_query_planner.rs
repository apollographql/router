//! Calls out to nodejs query planner

use std::fmt::Debug;
use std::sync::Arc;
use std::time::Instant;

use futures::future::BoxFuture;
use router_bridge::planner::IncrementalDeliverySupport;
use router_bridge::planner::PlanSuccess;
use router_bridge::planner::Planner;
use router_bridge::planner::QueryPlannerConfig;
use serde::Deserialize;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use tower::Service;

use super::PlanNode;
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
        mut query: Query,
        operation: Option<String>,
    ) -> Result<QueryPlannerContent, QueryPlannerError> {
        crate::spec::operation_limits::check(&self.configuration, &mut query, operation.clone())?;

        let planner_result = self
            .planner
            .plan(query.string.clone(), operation)
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
                query.subselections = subselections;
                Ok(QueryPlannerContent::Plan {
                    plan: Arc::new(super::QueryPlan {
                        usage_reporting,
                        root: node,
                        formatted_query_plan,
                        query: Arc::new(query),
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
            let res = this.get(req.query, req.operation_name.to_owned()).await;
            let duration = start.elapsed().as_secs_f64();
            tracing::info!(histogram.apollo_router_query_planning_time = duration,);

            match res {
                Ok(query_planner_content) => Ok(QueryPlannerResponse::builder()
                    .content(query_planner_content)
                    .context(req.context)
                    .build()),
                Err(e) => {
                    if let QueryPlannerError::PlanningErrors(pe) = &e {
                        req.context
                            .private_entries
                            .lock()
                            .insert(pe.usage_reporting.clone());
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
    async fn get(
        &self,
        query: Query,
        operation_name: Option<String>,
    ) -> Result<QueryPlannerContent, QueryPlannerError> {
        if query.contains_introspection() {
            // If we have only one operation containing only the root field `__typename`
            // (possibly aliased or repeated). (This does mean we fail to properly support
            // {"query": "query A {__typename} query B{somethingElse}", "operationName":"A"}.)
            if let Some(output_keys) = query
                .operations
                .get(0)
                .and_then(|op| op.is_only_typenames_with_output_keys())
            {
                let operation_name = query.operations[0].kind().to_string();
                let data: Value = Value::Object(Map::from_iter(
                    output_keys
                        .into_iter()
                        .map(|key| (key, Value::String(operation_name.clone().into()))),
                ));
                return Ok(QueryPlannerContent::Introspection {
                    response: Box::new(graphql::Response::builder().data(data).build()),
                });
            } else {
                return self.introspection(query.string.clone()).await;
            }
        }

        self.plan(query, operation_name).await
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
    use crate::services::layers::query_parsing::QueryParsingLayer;

    const EXAMPLE_SCHEMA: &str = include_str!("testdata/schema.graphql");

    #[test(tokio::test)]
    async fn test_plan() {
        let result = plan(EXAMPLE_SCHEMA, include_str!("testdata/query.graphql"), None)
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
            None,
        )
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
        let parser = QueryParsingLayer::new(
            Arc::new(Schema::parse(EXAMPLE_SCHEMA, &Default::default(), None).unwrap()),
            Arc::clone(&Default::default()),
        );

        let query = parser.parse((
            include_str!("testdata/unknown_introspection_query.graphql").into(),
            None,
        ));
        let err = BridgeQueryPlanner::new(EXAMPLE_SCHEMA.to_string(), Default::default())
            .await
            .unwrap()
            // test the planning part separately because it is a valid introspection query
            // it should be caught by the introspection part, but just in case, we check
            // that the query planner would return an empty plan error if it received an
            // introspection query
            .plan(query, None)
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
        let result = plan(EXAMPLE_SCHEMA, "", None).await;

        assert_eq!(
            "couldn't plan query: query validation errors: Syntax Error: Unexpected <EOF>.",
            result.unwrap_err().to_string()
        );
    }

    #[test(tokio::test)]
    async fn test_single_aliased_root_typename() {
        let result = plan(EXAMPLE_SCHEMA, "{ x: __typename }", None)
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
        let result = plan(EXAMPLE_SCHEMA, "{ x: __typename __typename }", None)
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

    async fn plan(
        schema: &str,
        query: &str,
        operation_name: Option<String>,
    ) -> Result<QueryPlannerContent, QueryPlannerError> {
        let mut configuration: Configuration = Default::default();
        configuration.supergraph.introspection = true;
        let configuration = Arc::new(configuration);
        let api_schema = None;
        let parser = QueryParsingLayer::new(
            Arc::new(Schema::parse(schema, &configuration, api_schema).unwrap()),
            Arc::clone(&configuration),
        );
        let planner = BridgeQueryPlanner::new(schema.to_string(), configuration)
            .await
            .unwrap();

        planner
            .get(
                parser.parse((query.to_string(), operation_name.clone())),
                operation_name,
            )
            .await
    }
}
