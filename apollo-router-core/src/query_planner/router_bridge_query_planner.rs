//! Calls out to nodejs query planner

use crate::prelude::graphql::*;
use async_trait::async_trait;
use router_bridge::plan;
use serde::Deserialize;
use std::sync::Arc;

/// A query planner that calls out to the nodejs router-bridge query planner.
///
/// No caching is performed. To cache, wrap in a [`CachingQueryPlanner`].
#[derive(Debug)]
pub struct RouterBridgeQueryPlanner {
    schema: Arc<Schema>,
}

impl RouterBridgeQueryPlanner {
    /// Create a new router-bridge query planner
    pub fn new(schema: Arc<Schema>) -> Self {
        Self { schema }
    }
}

#[async_trait]
impl QueryPlanner for RouterBridgeQueryPlanner {
    #[tracing::instrument(skip_all, name = "plan", level = "debug")]
    async fn get(
        &self,
        query: String,
        operation: Option<String>,
        options: QueryPlanOptions,
    ) -> Result<Arc<QueryPlan>, QueryPlannerError> {
        let context = plan::OperationalContext {
            schema: self.schema.as_str().to_string(),
            query,
            operation_name: operation.unwrap_or_default(),
        };

        let planner_result = tokio::task::spawn_blocking(|| {
            plan::plan::<PlannerResult>(context, options.into())
                .map_err(QueryPlannerError::RouterBridgeError)
        })
        .await???;

        match planner_result {
            PlannerResult::QueryPlan { node: Some(node) } => Ok(Arc::new(QueryPlan { root: node })),
            PlannerResult::QueryPlan { node: None } => {
                failfast_debug!("Empty query plan");
                Err(QueryPlannerError::EmptyPlan)
            }
            PlannerResult::Other => {
                failfast_debug!("Unhandled planner result");
                Err(QueryPlannerError::UnhandledPlannerResult)
            }
        }
    }
}

impl From<QueryPlanOptions> for plan::QueryPlanOptions {
    fn from(_: QueryPlanOptions) -> Self {
        plan::QueryPlanOptions::default()
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
        let planner = RouterBridgeQueryPlanner::new(Arc::new(
            include_str!("testdata/schema.graphql").parse().unwrap(),
        ));
        let result = planner
            .get(
                include_str!("testdata/query.graphql").into(),
                None,
                Default::default(),
            )
            .await
            .unwrap();
        insta::assert_debug_snapshot!(result);
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
            RouterBridgeQueryPlanner::new(Arc::new(
                include_str!("testdata/schema.graphql").parse().unwrap(),
            ))
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
        let planner = RouterBridgeQueryPlanner::new(Arc::new("".parse().unwrap()));
        let result = planner.get("".into(), None, Default::default()).await;

        assert_eq!(
            "Query planning had errors: Planning errors: UNKNOWN: Syntax Error: Unexpected <EOF>.",
            result.unwrap_err().to_string()
        );
    }
}
