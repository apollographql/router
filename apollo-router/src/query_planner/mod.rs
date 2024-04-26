//! GraphQL operation planning.

#![allow(missing_docs)] // FIXME

pub(crate) use bridge_query_planner::*;
pub(crate) use bridge_query_planner_pool::*;
pub(crate) use caching_query_planner::*;
pub use plan::QueryPlan;
pub(crate) use plan::*;

pub use self::fetch::OperationKind;

mod bridge_query_planner;
mod bridge_query_planner_pool;
mod caching_query_planner;
mod convert;
mod execution;
pub(crate) mod fetch;
mod labeler;
mod plan;
pub(crate) mod rewrites;
mod selection;
pub(crate) mod subscription;

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

// The code resides in a separate submodule to allow writing a log filter activating it
// separately from the query planner logs, as follows:
// `router -s supergraph.graphql --log info,crate::query_planner::log=trace`
mod log {
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Map;
    use serde_json_bytes::Value;

    use crate::query_planner::PlanNode;

    pub(crate) fn trace_query_plan(plan: &PlanNode) {
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
