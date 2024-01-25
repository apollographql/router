use crate::query_graph::graph_path::{
    ExcludedConditions, ExcludedDestinations, OpGraphPathContext,
};
use crate::query_graph::path_tree::OpPathTree;
use crate::query_plan::QueryPlanCost;
use petgraph::graph::EdgeIndex;

/// Note that `ConditionResolver`s are guaranteed to be only called for edge with conditions.
pub(crate) trait ConditionResolver {
    fn resolve(
        edge: EdgeIndex,
        context: OpGraphPathContext,
        excluded_destinations: ExcludedDestinations,
        excluded_conditions: ExcludedConditions,
    ) -> ConditionResolution;
}

pub(crate) struct ConditionResolution {
    satisfied: bool,
    cost: QueryPlanCost,
    path_tree: Option<OpPathTree>,
    // Note that this is not guaranteed to be set even if satistied === false.
    unsatisfied_condition_reason: Option<UnsatisfiedConditionReason>,
}

pub(crate) enum UnsatisfiedConditionReason {
    NoPostRequireKey,
}

pub(crate) struct CachingConditionResolver;

impl ConditionResolver for CachingConditionResolver {
    fn resolve(
        _edge: EdgeIndex,
        _context: OpGraphPathContext,
        _excluded_destinations: ExcludedDestinations,
        _excluded_conditions: ExcludedConditions,
    ) -> ConditionResolution {
        todo!()
    }
}
