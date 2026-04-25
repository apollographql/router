//! Adapter for the BASELINE `apollo-federation` from crates.io (renamed via
//! Cargo's `package = "apollo-federation"` to `apollo_federation_base`).
//!
//! Mirrors [`crate::harness_head::HeadPlanner`] but binds against a different
//! compiled version. Two versions co-exist in the same binary because they
//! are distinct Cargo packages from the linker's point of view.

use std::num::NonZeroU32;

use apollo_compiler::ExecutableDocument;
use apollo_federation_base::Supergraph;
use apollo_federation_base::query_plan::query_planner::QueryPlanIncrementalDeliveryConfig;
use apollo_federation_base::query_plan::query_planner::QueryPlanOptions;
use apollo_federation_base::query_plan::query_planner::QueryPlanner;
use apollo_federation_base::query_plan::query_planner::QueryPlannerConfig;
use apollo_federation_base::query_plan::query_planner::QueryPlannerDebugConfig;

use crate::harness::{CommonConfig, CommonOptions, HarnessError, PlannerHarness};

const VERSION: &str = "base";

pub struct BasePlanner {
    inner: QueryPlanner,
}

impl PlannerHarness for BasePlanner {
    fn version_label() -> &'static str {
        VERSION
    }

    fn build(supergraph_sdl: &str, cfg: &CommonConfig) -> Result<Self, HarnessError> {
        // 2.1.3 lacks `new_with_router_specs`. Both fall back to `new`,
        // which uses the default supported-spec set.
        let supergraph = Supergraph::new(supergraph_sdl).map_err(|e| HarnessError::Supergraph {
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

        // QueryPlanOptions shape varies across versions. Using struct-update
        // syntax means we set the field that's stable across the 2.x series
        // (`override_conditions`) and let the version's `Default` fill in
        // the rest. This is exactly the kind of drift the adapter absorbs.
        let _ = (&opts.disabled_subgraph_names, opts.non_local_selections_limit);
        let options = QueryPlanOptions {
            override_conditions: opts.override_conditions.clone(),
            ..Default::default()
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
