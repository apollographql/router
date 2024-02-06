use crate::error::FederationError;
use crate::query_graph::graph_path::{
    ExcludedConditions, ExcludedDestinations, OpGraphPathContext,
};
use crate::query_graph::path_tree::OpPathTree;
use crate::query_plan::QueryPlanCost;
use petgraph::graph::EdgeIndex;
use std::sync::Arc;

/// Note that `ConditionResolver`s are guaranteed to be only called for edge with conditions.
pub(crate) trait ConditionResolver {
    fn resolve(
        &mut self,
        edge: EdgeIndex,
        context: &OpGraphPathContext,
        excluded_destinations: &ExcludedDestinations,
        excluded_conditions: &ExcludedConditions,
    ) -> Result<ConditionResolution, FederationError>;
}

#[derive(Debug, Clone)]
pub(crate) enum ConditionResolution {
    Satisfied {
        cost: QueryPlanCost,
        path_tree: Option<Arc<OpPathTree>>,
    },
    Unsatisfied {
        reason: Option<UnsatisfiedConditionReason>,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum UnsatisfiedConditionReason {
    NoPostRequireKey,
}

impl ConditionResolution {
    pub(crate) fn no_conditions() -> Self {
        Self::Satisfied {
            cost: 0,
            path_tree: None,
        }
    }

    pub(crate) fn unsatisfied_conditions() -> Self {
        Self::Unsatisfied { reason: None }
    }
}

pub(crate) struct CachingConditionResolver;

impl ConditionResolver for CachingConditionResolver {
    fn resolve(
        &mut self,
        _edge: EdgeIndex,
        _context: &OpGraphPathContext,
        _excluded_destinations: &ExcludedDestinations,
        _excluded_conditions: &ExcludedConditions,
    ) -> Result<ConditionResolution, FederationError> {
        todo!()
    }
}
