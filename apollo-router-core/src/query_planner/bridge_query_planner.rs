//! Calls out to nodejs query planner

use crate::prelude::graphql::*;
use async_trait::async_trait;
use futures::future::BoxFuture;
use router_bridge::planner::Planner;
use serde::Deserialize;
use std::fmt::Debug;
use std::sync::Arc;
use tower::BoxError;
use tower::Service;

#[derive(Deserialize, Debug)]
struct BridgePlannerResult {
    data: Option<PlannerResult>,
    error: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
/// A query planner that calls out to the nodejs router-bridge query planner.
///
/// No caching is performed. To cache, wrap in a [`CachingQueryPlanner`].
pub struct BridgeQueryPlanner {
    planner: Arc<Planner<BridgePlannerResult>>,
}

impl BridgeQueryPlanner {
    pub async fn new(schema: Arc<Schema>) -> Self {
        Self {
            // TODO: Error handling
            planner: Arc::new(
                Planner::new(schema.as_str().to_string())
                    .await
                    .expect("couldn't instantiate query planner"),
            ),
        }
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
            let body = req.context.request.body();
            match this
                .get(
                    body.query.clone().expect(
                        "presence of a query has been checked by the RouterService before; qed",
                    ),
                    body.operation_name.to_owned(),
                    Default::default(),
                )
                .await
            {
                Ok(query_plan) => Ok(QueryPlannerResponse {
                    query_plan,
                    context: req.context,
                }),
                Err(e) => Err(tower::BoxError::from(e)),
            }
        };

        // Return the response as an immediate future
        Box::pin(fut)
    }
}

#[async_trait]
impl QueryPlanner for BridgeQueryPlanner {
    #[tracing::instrument(skip_all, level = "info", name = "plan")]
    async fn get(
        &self,
        query: String,
        operation: Option<String>,
        _options: QueryPlanOptions,
    ) -> Result<Arc<QueryPlan>, QueryPlannerError> {
        let planner_result: BridgePlannerResult = self
            .planner
            .plan(query, operation)
            .await
            .map_err(QueryPlannerError::RouterBridgeError)??;

        if let Some(data) = planner_result.data {
            match data {
                PlannerResult::QueryPlan { node: Some(node) } => {
                    Ok(Arc::new(QueryPlan { root: node }))
                }
                PlannerResult::QueryPlan { node: None } => {
                    failfast_debug!("empty query plan");
                    Err(QueryPlannerError::EmptyPlan)
                }
                PlannerResult::Other => {
                    failfast_debug!("unhandled planner result");
                    Err(QueryPlannerError::UnhandledPlannerResult)
                }
            }
        } else {
            failfast_debug!("unhandled planner result");
            Err(QueryPlannerError::PlannerErrors(
                planner_result.error.unwrap_or(serde_json::Value::Null),
            ))
        }
    }
}

/// The root query plan container.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "kind")]
enum PlannerResult {
    QueryPlan {
        /// The hierarchical nodes that make up the query plan
        node: Option<PlanNode>,
    },
    #[serde(other)]
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use test_log::test;

    #[test(tokio::test)]
    async fn test_plan() {
        let planner = BridgeQueryPlanner::new(Arc::new(example_schema())).await;
        let result = planner
            .get(
                include_str!("testdata/query.graphql").into(),
                None,
                Default::default(),
            )
            .await
            .unwrap();
        insta::assert_debug_snapshot!("plan", result);
    }

    fn example_schema() -> Schema {
        include_str!("testdata/schema.graphql").parse().unwrap()
    }

    #[test]
    fn empty_query_plan() {
        serde_json::from_value::<PlannerResult>(json!({ "kind": "QueryPlan"})).expect(
            "If this test fails, It probably means QueryPlan::node isn't an Option anymore.\n
                 Introspection queries return an empty QueryPlan, so the node field needs to remain optional.",
        );
    }

    #[test(tokio::test)]
    async fn empty_query_plan_should_be_a_planner_error() {
        insta::assert_debug_snapshot!(
            "empty_query_plan_should_be_a_planner_error",
            BridgeQueryPlanner::new(Arc::new(example_schema()))
                .await
                .get(
                    include_str!("testdata/unknown_introspection_query.graphql").into(),
                    None,
                    Default::default(),
                )
                .await
        )
    }

    #[test(tokio::test)]
    async fn test_plan_error() {
        let planner = BridgeQueryPlanner::new(Arc::new(example_schema())).await;
        let result = planner.get("".into(), None, Default::default()).await;

        assert_eq!(
            "Query planning had errors: Planning errors: UNKNOWN: Syntax Error: Unexpected <EOF>.",
            result.unwrap_err().to_string()
        );
    }
}
