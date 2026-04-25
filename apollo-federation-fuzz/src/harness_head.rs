//! Adapter for the in-tree (HEAD) `apollo-federation` crate.
//!
//! Translates the neutral [`CommonConfig`]/[`CommonOptions`] from
//! [`crate::harness`] into the HEAD version's concrete planner config types,
//! runs the planner, and serializes the resulting `QueryPlan` to JSON.

use std::num::NonZeroU32;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::collections::IndexSet;
use apollo_federation::Supergraph;
use apollo_federation::query_plan::query_planner::QueryPlanIncrementalDeliveryConfig;
use apollo_federation::query_plan::query_planner::QueryPlanOptions;
use apollo_federation::query_plan::query_planner::QueryPlanner;
use apollo_federation::query_plan::query_planner::QueryPlannerConfig;
use apollo_federation::query_plan::query_planner::QueryPlannerDebugConfig;

use crate::harness::{CommonConfig, CommonOptions, HarnessError, PlannerHarness};

const VERSION: &str = "head";

pub struct HeadPlanner {
    inner: QueryPlanner,
}

impl PlannerHarness for HeadPlanner {
    fn version_label() -> &'static str {
        VERSION
    }

    fn build(supergraph_sdl: &str, cfg: &CommonConfig) -> Result<Self, HarnessError> {
        let supergraph =
            Supergraph::new_with_router_specs(supergraph_sdl).map_err(|e| HarnessError::Supergraph {
                version: VERSION,
                detail: e.to_string(),
            })?;

        let max_evaluated_plans =
            NonZeroU32::new(cfg.max_evaluated_plans.max(1)).unwrap_or(NonZeroU32::new(1).unwrap());

        let planner_cfg = QueryPlannerConfig {
            generate_query_fragments: cfg.generate_query_fragments,
            subgraph_graphql_validation: cfg.subgraph_validation,
            incremental_delivery: QueryPlanIncrementalDeliveryConfig {
                enable_defer: cfg.incremental_delivery,
            },
            debug: QueryPlannerDebugConfig {
                max_evaluated_plans,
                paths_limit: None,
            },
            type_conditioned_fetching: cfg.type_conditioned_fetching,
        };

        let inner =
            QueryPlanner::new(&supergraph, planner_cfg).map_err(|e| HarnessError::Construct {
                version: VERSION,
                detail: e.to_string(),
            })?;

        Ok(Self { inner })
    }

    fn plan(
        &self,
        operation: &str,
        operation_name: Option<&str>,
        opts: &CommonOptions,
    ) -> Result<serde_json::Value, HarnessError> {
        let api_schema = self.inner.api_schema();
        let document = ExecutableDocument::parse_and_validate(
            api_schema.schema(),
            operation,
            "operation.graphql",
        )
        .map_err(|e| HarnessError::Operation {
            version: VERSION,
            detail: e.to_string(),
        })?;

        let op_name = operation_name
            .map(|n| {
                apollo_compiler::Name::new(n).map_err(|e| HarnessError::Operation {
                    version: VERSION,
                    detail: format!("invalid operation name: {e}"),
                })
            })
            .transpose()?;

        let options = QueryPlanOptions {
            override_conditions: opts.override_conditions.clone(),
            check_for_cooperative_cancellation: None,
            non_local_selections_limit_enabled: opts.non_local_selections_limit,
            disabled_subgraph_names: opts.disabled_subgraph_names.iter().cloned().collect::<IndexSet<_>>(),
        };

        let plan = self
            .inner
            .build_query_plan(&document, op_name, options)
            .map_err(|e| HarnessError::Plan {
                version: VERSION,
                detail: e.to_string(),
            })?;

        serde_json::to_value(&plan).map_err(|e| HarnessError::Serialize {
            version: VERSION,
            detail: e.to_string(),
        })
    }
}
