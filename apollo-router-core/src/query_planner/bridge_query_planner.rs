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

#[derive(Debug, Clone)]
/// A query planner that calls out to the nodejs router-bridge query planner.
///
/// No caching is performed. To cache, wrap in a [`CachingQueryPlanner`].
pub struct BridgeQueryPlanner {
    planner: Arc<Planner<PlannerResult>>,
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
                    Default::default(),
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
        _options: QueryPlanOptions,
    ) -> Result<Arc<QueryPlan>, QueryPlannerError> {
        let planner_result = self
            .planner
            .plan(query, operation)
            .await
            .map_err(QueryPlannerError::RouterBridgeError)?
            .into_result()
            .map_err(QueryPlannerError::from)?;

        match planner_result {
            PlannerResult::QueryPlan { node: Some(node) } => Ok(Arc::new(QueryPlan { root: node })),
            PlannerResult::QueryPlan { node: None } => {
                failfast_debug!("empty query plan");
                Err(QueryPlannerError::EmptyPlan)
            }
            PlannerResult::Other => {
                failfast_debug!("unhandled planner result");
                Err(QueryPlannerError::UnhandledPlannerResult)
            }
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
                .unwrap()
                .get(
                    include_str!("testdata/unknown_introspection_query.graphql").into(),
                    None,
                    Default::default(),
                )
                .await
                .unwrap_err()
        )
    }

    #[test(tokio::test)]
    async fn test_plan_error() {
        let planner = BridgeQueryPlanner::new(Arc::new(example_schema()))
            .await
            .unwrap();
        let result = planner.get("".into(), None, Default::default()).await;

        assert_eq!(
            "couldn't plan query: query validation errors: UNKNOWN: Syntax Error: Unexpected <EOF>.",
            result.unwrap_err().to_string()
        );
    }
}
