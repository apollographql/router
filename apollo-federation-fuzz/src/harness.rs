//! Version-agnostic planner harness.
//!
//! Both adapters ([`crate::HeadPlanner`] and [`crate::BasePlanner`]) translate
//! these neutral types into their respective `apollo-federation` version's
//! concrete config/option/plan structs and serialize the resulting plan to
//! JSON. The diff layer only ever sees `serde_json::Value`, so it doesn't
//! depend on either version's type names.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("supergraph parse/validation failed ({version}): {detail}")]
    Supergraph {
        version: &'static str,
        detail: String,
    },
    #[error("query planner construction failed ({version}): {detail}")]
    Construct {
        version: &'static str,
        detail: String,
    },
    #[error("operation parse/validation failed ({version}): {detail}")]
    Operation {
        version: &'static str,
        detail: String,
    },
    #[error("plan generation failed ({version}): {detail}")]
    Plan {
        version: &'static str,
        detail: String,
    },
    #[error("plan serialization failed ({version}): {detail}")]
    Serialize {
        version: &'static str,
        detail: String,
    },
}

/// Neutral knobs we expose to the diff layer. Each adapter maps these onto
/// its own version's `QueryPlannerConfig`, defaulting any field that doesn't
/// exist on that side.
#[derive(Debug, Clone)]
pub struct CommonConfig {
    pub max_evaluated_plans: u32,
    pub generate_query_fragments: bool,
    pub type_conditioned_fetching: bool,
    pub incremental_delivery: bool,
    pub subgraph_validation: bool,
}

impl Default for CommonConfig {
    fn default() -> Self {
        Self {
            max_evaluated_plans: 10_000,
            generate_query_fragments: false,
            type_conditioned_fetching: false,
            incremental_delivery: false,
            subgraph_validation: false,
        }
    }
}

/// Neutral planner-options. Mapped per-adapter onto each version's
/// `QueryPlanOptions`.
#[derive(Debug, Clone, Default)]
pub struct CommonOptions {
    pub override_conditions: Vec<String>,
    pub disabled_subgraph_names: Vec<String>,
    pub non_local_selections_limit: bool,
}

/// A version-agnostic planner.
pub trait PlannerHarness: Sized {
    /// Build a planner from a composed supergraph SDL string.
    fn build(supergraph_sdl: &str, cfg: &CommonConfig) -> Result<Self, HarnessError>;

    /// Plan an operation. The returned `serde_json::Value` is the version's
    /// own `QueryPlan` serialized via serde — the only common ground between
    /// versions whose Rust types may diverge.
    fn plan(
        &self,
        operation: &str,
        operation_name: Option<&str>,
        opts: &CommonOptions,
    ) -> Result<serde_json::Value, HarnessError>;

    /// Static label used in error messages and diff output.
    fn version_label() -> &'static str;
}
