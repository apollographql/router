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
pub(crate) mod lookup;
#[cfg(test)]
mod lookup_tests;
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
/// Connector fetches carry their connector's synthetic service name, and their operations
/// are written against the source subgraph's schema, so each synthetic name aliases that
/// subgraph's entry.
pub(crate) fn build_subgraph_schemas(planner: &QueryPlanner) -> Arc<SubgraphSchemas> {
    let mut schemas: SubgraphSchemas = planner
        .subgraph_schemas()
        .iter()
        .map(|(name, schema)| (name.to_string(), Arc::new(schema.schema().clone())))
        .collect();
    for (service_name, subgraph_name) in planner.connector_index().service_subgraphs() {
        if let Some(schema) = schemas.get(subgraph_name).cloned() {
            schemas.insert(service_name.to_string(), schema);
        }
    }
    Arc::new(schemas)
}

/// Subgraph schemas with their precomputed schema hash, keyed by subgraph name.
///
/// This is only needed by the query planner service, to compute schema-aware operation
/// hashes for fetch nodes. Elsewhere in the router, use [`SubgraphSchemas`] instead.
type HashedSubgraphSchemas = HashMap<String, HashedSubgraphSchema>;

/// Returns a map of subgraph schemas and hashes for each. Synthetic connector
/// service names alias their source subgraph's entry, as in
/// [`build_subgraph_schemas`].
fn hashed_subgraph_schemas(planner: &QueryPlanner) -> Arc<HashedSubgraphSchemas> {
    let mut schemas: HashedSubgraphSchemas = planner
        .subgraph_schemas()
        .iter()
        .map(|(name, schema)| {
            (
                name.to_string(),
                HashedSubgraphSchema::new(schema.schema().clone()),
            )
        })
        .collect();
    for (service_name, subgraph_name) in planner.connector_index().service_subgraphs() {
        if let Some(schema) = schemas.get(subgraph_name).cloned() {
            schemas.insert(service_name.to_string(), schema);
        }
    }
    Arc::new(schemas)
}

#[derive(Clone)]
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
