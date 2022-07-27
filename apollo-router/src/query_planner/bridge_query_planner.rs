//! Calls out to nodejs query planner

use std::fmt::Debug;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use opentelemetry::trace::SpanKind;
use router_bridge::planner::DeferStreamSupport;
use router_bridge::planner::PlanSuccess;
use router_bridge::planner::Planner;
use router_bridge::planner::QueryPlannerConfig;
use serde::Deserialize;
use tower::BoxError;
use tower::Service;
use tracing::Instrument;

use super::PlanNode;
use super::QueryPlanOptions;
use crate::error::QueryPlannerError;
use crate::introspection::Introspection;
use crate::services::QueryPlannerContent;
use crate::traits::QueryKey;
use crate::traits::QueryPlanner;
use crate::*;

pub(crate) static USAGE_REPORTING: &str = "apollo_telemetry::usage_reporting";

#[derive(Debug, Clone)]
/// A query planner that calls out to the nodejs router-bridge query planner.
///
/// No caching is performed. To cache, wrap in a [`CachingQueryPlanner`].
pub(crate) struct BridgeQueryPlanner {
    planner: Arc<Planner<QueryPlan>>,
    schema: Arc<Schema>,
    introspection: Option<Arc<Introspection>>,
}

impl BridgeQueryPlanner {
    pub(crate) async fn new(
        schema: Arc<Schema>,
        introspection: Option<Arc<Introspection>>,
        defer_support: bool,
    ) -> Result<Self, QueryPlannerError> {
        Ok(Self {
            planner: Arc::new(
                Planner::new(
                    schema.as_str().to_string(),
                    QueryPlannerConfig {
                        defer_stream_support: Some(DeferStreamSupport {
                            enable_defer: Some(defer_support),
                        }),
                    },
                )
                .await?,
            ),
            schema,
            introspection,
        })
    }

    async fn parse_selections(&self, query: String) -> Result<Query, QueryPlannerError> {
        let schema = self.schema.clone();
        let query_parsing_future =
            tokio::task::spawn_blocking(move || Query::parse(query, &schema))
                .instrument(tracing::info_span!("parse_query", "otel.kind" = %SpanKind::Internal));
        match query_parsing_future.await {
            Ok(res) => res.map_err(QueryPlannerError::from),
            Err(err) => {
                failfast_debug!("parsing query task failed: {}", err);
                Err(QueryPlannerError::from(err))
            }
        }
    }

    async fn introspection(&self, query: &str) -> Result<QueryPlannerContent, QueryPlannerError> {
        match self.introspection.as_ref() {
            Some(introspection) => {
                let response = introspection
                    .execute(self.schema.as_str(), query)
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
        options: QueryPlanOptions,
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
                data: QueryPlan { node: Some(node) },
                usage_reporting,
            } => {
                let subselections = node.parse_subselections(&*self.schema);
                selections.subselections = subselections;
                Ok(QueryPlannerContent::Plan {
                    plan: Arc::new(query_planner::QueryPlan {
                        usage_reporting,
                        root: node,
                        options,
                    }),
                    query: Arc::new(selections),
                })
            }
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
            match this
                .get((
                    req.query.clone(),
                    req.operation_name.to_owned(),
                    req.query_plan_options,
                ))
                .await
            {
                Ok(query_planner_content) => Ok(QueryPlannerResponse::new(
                    query_planner_content,
                    req.context,
                )),
                Err(e) => Err(tower::BoxError::from(e)),
            }
        };

        // Return the response as an immediate future
        Box::pin(fut)
    }
}

#[async_trait]
impl QueryPlanner for BridgeQueryPlanner {
    async fn get(&self, key: QueryKey) -> Result<QueryPlannerContent, QueryPlannerError> {
        let selections = self.parse_selections(key.0.clone()).await?;

        if selections.contains_introspection() {
            return self.introspection(key.0.as_str()).await;
        }

        self.plan(key.0, key.1, key.2, selections).await
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
    use serde_json::json;
    use test_log::test;

    use super::*;

    #[test(tokio::test)]
    async fn test_plan() {
        let planner = BridgeQueryPlanner::new(
            Arc::new(example_schema()),
            Some(Arc::new(Introspection::from_schema(&example_schema()))),
            false,
        )
        .await
        .unwrap();
        let result = planner
            .get((
                include_str!("testdata/query.graphql").into(),
                None,
                Default::default(),
            ))
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
        let planner = BridgeQueryPlanner::new(
            Arc::new(example_schema()),
            Some(Arc::new(Introspection::from_schema(&example_schema()))),
            false,
        )
        .await
        .unwrap();
        let err = planner
            .get((
                "fragment UnusedTestFragment on User { id } query { me { id } }".to_string(),
                None,
                Default::default(),
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
        let err = BridgeQueryPlanner::new(
            Arc::new(example_schema()),
            Some(Arc::new(Introspection::from_schema(&example_schema()))),
            false,
        )
        .await
        .unwrap()
        // test the planning part separately because it is a valid introspection query
        // it should be caught by the introspection part, but just in case, we check
        // that the query planner would return an empty plan error if it received an
        // introspection query
        .plan(
            include_str!("testdata/unknown_introspection_query.graphql").into(),
            None,
            QueryPlanOptions::default(),
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
                panic!("empty plan should have returned an EmptyPlanError: {:?}", e);
            }
        }
    }

    #[test(tokio::test)]
    async fn test_plan_error() {
        let planner = BridgeQueryPlanner::new(
            Arc::new(example_schema()),
            Some(Arc::new(Introspection::from_schema(&example_schema()))),
            false,
        )
        .await
        .unwrap();
        let result = planner.get(("".into(), None, Default::default())).await;

        assert_eq!(
            "couldn't plan query: query validation errors: Syntax Error: Unexpected <EOF>.",
            result.unwrap_err().to_string()
        );
    }
}
