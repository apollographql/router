//! GraphQL operation planning.

#![allow(missing_docs)] // FIXME

use std::sync::Arc;

use apollo_compiler::collections::HashMap;
use apollo_compiler::validation::Valid;
use apollo_federation::query_plan::query_planner::QueryPlanner;
pub(crate) use caching_query_planner::*;
pub use plan::QueryPlan;
pub(crate) use plan::*;
pub(crate) use query_planner_service::*;
pub(crate) use subgraph_context::build_operation_with_aliasing;

pub use self::fetch::OperationKind;
use crate::spec::SchemaHash;

mod caching_query_planner;
mod convert;
mod execution;
pub(crate) mod fetch;
mod labeler;
mod plan;
pub(crate) mod query_planner_service;
pub(crate) mod rewrites;
pub(crate) mod selection;
mod subgraph_context;
pub(crate) mod subscription;
pub(crate) mod warmup;

pub(crate) const FETCH_SPAN_NAME: &str = "fetch";
pub(crate) const SUBSCRIBE_SPAN_NAME: &str = "subscribe";
pub(crate) const FLATTEN_SPAN_NAME: &str = "flatten";
pub(crate) const SEQUENCE_SPAN_NAME: &str = "sequence";
pub(crate) const PARALLEL_SPAN_NAME: &str = "parallel";
pub(crate) const DEFER_SPAN_NAME: &str = "defer";
pub(crate) const DEFER_PRIMARY_SPAN_NAME: &str = "defer_primary";
pub(crate) const DEFER_DEFERRED_SPAN_NAME: &str = "defer_deferred";
pub(crate) const CONDITION_SPAN_NAME: &str = "condition";
pub(crate) const CONDITION_IF_SPAN_NAME: &str = "condition_if";
pub(crate) const CONDITION_ELSE_SPAN_NAME: &str = "condition_else";

/// Subgraph schemas, keyed by subgraph name.
pub(crate) type SubgraphSchemas = HashMap<String, Arc<Valid<apollo_compiler::Schema>>>;

/// Returns a map of the subgraph schemas known to the given planner, keyed by subgraph name.
pub(crate) fn build_subgraph_schemas(planner: &QueryPlanner) -> Arc<SubgraphSchemas> {
    Arc::new(
        planner
            .subgraph_schemas()
            .iter()
            .map(|(name, schema)| (name.to_string(), Arc::new(schema.schema().clone())))
            .collect(),
    )
}

/// Subgraph schemas with their precomputed schema hash, keyed by subgraph name.
///
/// This is only needed by the query planner service, to compute schema-aware operation
/// hashes for fetch nodes. Elsewhere in the router, use [`SubgraphSchemas`] instead.
type HashedSubgraphSchemas = HashMap<String, HashedSubgraphSchema>;

/// Returns a map of subgraph schemas and hashes for each.
fn hashed_subgraph_schemas(planner: &QueryPlanner) -> Arc<HashedSubgraphSchemas> {
    Arc::new(
        planner
            .subgraph_schemas()
            .iter()
            .map(|(name, schema)| {
                (
                    name.to_string(),
                    HashedSubgraphSchema::new(schema.schema().clone()),
                )
            })
            .collect(),
    )
}

struct HashedSubgraphSchema {
    schema: Arc<Valid<apollo_compiler::Schema>>,
    hash: SchemaHash,
}

impl HashedSubgraphSchema {
    fn new(schema: Valid<apollo_compiler::Schema>) -> Self {
        let sdl = schema.serialize().no_indent().to_string();
        Self {
            schema: Arc::new(schema),
            hash: SchemaHash::new(&sdl),
        }
    }
}

// The code resides in a separate submodule to allow writing a log filter activating it
// separately from the query planner logs, as follows:
// `router -s supergraph.graphql --log info,crate::query_planner::log=trace`
mod log {
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Map;
    use serde_json_bytes::Value;

    use crate::query_planner::PlanNode;

    pub(crate) fn trace_query_plan(plan: Option<&PlanNode>) {
        tracing::trace!("query plan\n{:?}", plan);
    }

    pub(crate) fn trace_subfetch(
        service_name: &str,
        operation: &str,
        variables: &Map<ByteString, Value>,
        response: &crate::graphql::Response,
    ) {
        tracing::trace!(
            "subgraph fetch to {}: operation = '{}', variables = {:?}, response:\n{}",
            service_name,
            operation,
            variables,
            serde_json::to_string_pretty(&response).unwrap()
        );
    }
}

#[cfg(test)]
mod tests;
