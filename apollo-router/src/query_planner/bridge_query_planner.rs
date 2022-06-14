//! Calls out to nodejs query planner

use super::PlanNode;
use super::QueryPlanOptions;
use crate::error::QueryPlannerError;
use crate::*;
use async_trait::async_trait;
use futures::future::BoxFuture;
use router_bridge::planner::PlanSuccess;
use router_bridge::planner::Planner;
use serde::Deserialize;
use std::fmt::Debug;
use std::sync::Arc;
use tower::BoxError;
use tower::Service;

pub static USAGE_REPORTING: &str = "apollo_telemetry::usage_reporting";

#[derive(Debug, Clone)]
/// A query planner that calls out to the nodejs router-bridge query planner.
///
/// No caching is performed. To cache, wrap in a [`CachingQueryPlanner`].
pub struct BridgeQueryPlanner {
    planner: Arc<Planner<QueryPlan>>,
}

impl BridgeQueryPlanner {
    pub async fn new(schema: Arc<Schema>) -> Result<Self, QueryPlannerError> {
        Ok(Self {
            planner: Arc::new(Planner::new(schema.as_str().to_string()).await?),
        })
    }
}

impl Service<QueryPlannerRequest> for BridgeQueryPlanner {
    type Response = QueryPlannerResponse;

    type Error = BoxError;

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
            let body = req.originating_request.body();
            match this
                .get(
                    body.query.clone().expect(
                        "presence of a query has been checked by the RouterService before; qed",
                    ),
                    body.operation_name.to_owned(),
                    req.query_plan_options,
                )
                .await
            {
                Ok(query_plan) => Ok(QueryPlannerResponse::new(query_plan, req.context)),
                Err(e) => Err(tower::BoxError::from(e)),
            }
        };

        // Return the response as an immediate future
        Box::pin(fut)
    }
}

#[async_trait]
impl QueryPlanner for BridgeQueryPlanner {
    async fn get(
        &self,
        query: String,
        operation: Option<String>,
        options: QueryPlanOptions,
    ) -> Result<Arc<query_planner::QueryPlan>, QueryPlannerError> {
        let planner_result = self
            .planner
            .plan(query, operation)
            .await
            .map_err(QueryPlannerError::RouterBridgeError)?
            .into_result()
            .map_err(QueryPlannerError::from)?;

        match planner_result {
            PlanSuccess {
                data: QueryPlan { node: Some(node) },
                usage_reporting,
            } => Ok(Arc::new(query_planner::QueryPlan {
                usage_reporting,
                root: node,
                options,
            })),
            PlanSuccess {
                data: QueryPlan { node: None },
                usage_reporting,
            } => {
                failfast_debug!("empty query plan");
                Err(QueryPlannerError::EmptyPlan(usage_reporting))
            }
        }
    }
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
    use super::*;
    use serde_json::json;
    use test_log::test;

    #[test(tokio::test)]
    async fn test_plan() {
        let planner = BridgeQueryPlanner::new(Arc::new(example_schema()))
            .await
            .unwrap();
        let result = planner
            .get(
                include_str!("testdata/query.graphql").into(),
                None,
                Default::default(),
            )
            .await
            .unwrap();
        insta::with_settings!({sort_maps => true}, {
            insta::assert_json_snapshot!("plan_usage_reporting", result.usage_reporting);
        });
        insta::assert_debug_snapshot!("plan_root", result.root);
    }

    #[test(tokio::test)]
    async fn test_plan_invalid_query() {
        let planner = BridgeQueryPlanner::new(Arc::new(example_schema()))
            .await
            .unwrap();
        let err = planner
            .get(
                "fragment UnusedTestFragment on User { id } query { me { id } }".to_string(),
                None,
                Default::default(),
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

    fn example_schema() -> Schema {
        include_str!("testdata/schema.graphql").parse().unwrap()
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
        let err = BridgeQueryPlanner::new(Arc::new(example_schema()))
            .await
            .unwrap()
            .get(
                include_str!("testdata/unknown_introspection_query.graphql").into(),
                None,
                Default::default(),
            )
            .await
            .unwrap_err();

        match err {
            QueryPlannerError::EmptyPlan(usage_reporting) => {
                insta::with_settings!({sort_maps => true}, {
                    insta::assert_json_snapshot!("empty_query_plan_usage_reporting", usage_reporting);
                });
            }
            _ => {
                panic!("empty plan should have returned an EmptyPlanError");
            }
        }
    }

    #[test(tokio::test)]
    async fn test_plan_error() {
        let planner = BridgeQueryPlanner::new(Arc::new(example_schema()))
            .await
            .unwrap();
        let result = planner.get("".into(), None, Default::default()).await;

        assert_eq!(
            "couldn't plan query: query validation errors: Syntax Error: Unexpected <EOF>.",
            result.unwrap_err().to_string()
        );
    }
}
