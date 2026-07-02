use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Type;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use petgraph::graph::EdgeIndex;

use crate::error::FederationError;
use crate::operation::SelectionSet;
use crate::query_graph::QueryGraph;
use crate::query_graph::graph_path::ExcludedConditions;
use crate::query_graph::graph_path::ExcludedDestinations;
use crate::query_graph::graph_path::operation::OpGraphPathContext;
use crate::query_graph::path_tree::OpPathTree;
use crate::query_plan::QueryPlanCost;

#[derive(Debug, Clone)]
pub(crate) struct ContextMapEntry {
    pub(crate) levels_in_data_path: usize,
    pub(crate) levels_in_query_path: usize,
    pub(crate) path_tree: Option<Arc<OpPathTree>>,
    pub(crate) selection_set: SelectionSet,
    // PORT_NOTE: This field was renamed from the JS name (`paramName`) to better align with naming
    // in ContextCondition.
    pub(crate) argument_name: Name,
    // PORT_NOTE: This field was renamed from the JS name (`argType`) to better align with naming in
    // ContextCondition.
    pub(crate) argument_type: Node<Type>,
    pub(crate) context_id: Name,
}

/// Note that `ConditionResolver`s are guaranteed to be only called for edge with conditions.
pub(crate) trait ConditionResolver {
    fn resolve(
        &mut self,
        edge: EdgeIndex,
        context: &OpGraphPathContext,
        excluded_destinations: &ExcludedDestinations,
        excluded_conditions: &ExcludedConditions,
        extra_conditions: Option<&SelectionSet>,
    ) -> Result<ConditionResolution, FederationError>;
}

#[derive(Debug, Clone)]
pub(crate) enum ConditionResolution {
    Satisfied {
        cost: QueryPlanCost,
        path_tree: Option<Arc<OpPathTree>>,
        context_map: Option<IndexMap<Name, ContextMapEntry>>,
    },
    Unsatisfied {
        reason: Option<UnsatisfiedConditionReason>,
    },
}

impl std::fmt::Display for ConditionResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConditionResolution::Satisfied {
                cost,
                path_tree,
                context_map,
            } => {
                writeln!(f, "Satisfied: cost={cost}")?;
                if let Some(path_tree) = path_tree {
                    writeln!(f, "path_tree:\n{path_tree}")?;
                }
                if let Some(context_map) = context_map {
                    writeln!(f, ", context_map:\n{context_map:?}")?;
                }
                Ok(())
            }
            ConditionResolution::Unsatisfied { reason } => {
                writeln!(f, "Unsatisfied: reason={reason:?}")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum UnsatisfiedConditionReason {
    NoPostRequireKey,
    NoSetContext,
}

impl ConditionResolution {
    pub(crate) fn no_conditions() -> Self {
        Self::Satisfied {
            cost: 0.0,
            path_tree: None,
            context_map: None,
        }
    }

    pub(crate) fn unsatisfied_conditions() -> Self {
        Self::Unsatisfied { reason: None }
    }
}

#[derive(Debug, derive_more::IsVariant)]
pub(crate) enum ConditionResolutionCacheResult {
    /// Cache hit.
    Hit(ConditionResolution),
    /// Cache miss; can be inserted into cache.
    Miss,
    /// The value can't be cached; Or, an incompatible value is already in cache.
    NotApplicable,
}

struct CachedConditionEntry {
    resolution: ConditionResolution,
    context: OpGraphPathContext,
    excluded_destinations: ExcludedDestinations,
    excluded_conditions: ExcludedConditions,
    used_subgraphs: IndexSet<Arc<str>>,
}

pub(crate) struct ConditionResolverCache {
    edge_states: IndexMap<EdgeIndex, Vec<CachedConditionEntry>>,
}

impl ConditionResolverCache {
    pub(crate) fn new() -> Self {
        Self {
            edge_states: Default::default(),
        }
    }

    pub(crate) fn contains(
        &mut self,
        edge: EdgeIndex,
        context: &OpGraphPathContext,
        excluded_destinations: &ExcludedDestinations,
        excluded_conditions: &ExcludedConditions,
        extra_conditions: Option<&SelectionSet>,
    ) -> ConditionResolutionCacheResult {
        if extra_conditions.is_some() {
            return ConditionResolutionCacheResult::NotApplicable;
        }

        if let Some(entries) = self.edge_states.get(&edge) {
            for cached in entries {
                let destinations_match = &cached.excluded_destinations == excluded_destinations;
                let conditions_match = &cached.excluded_conditions == excluded_conditions;

                if &cached.context == context && destinations_match && conditions_match {
                    return ConditionResolutionCacheResult::Hit(cached.resolution.clone());
                }

                // Path-aware satisfied reuse: if the cached resolution's path tree doesn't
                // traverse any of the newly-excluded destinations, the path is still
                // available and the cached result is valid. We require exact match on
                // excluded conditions because fewer condition exclusions could open up
                // better paths that weren't explored when the cached result was computed.
                if &cached.context == context
                    && conditions_match
                    && matches!(&cached.resolution, ConditionResolution::Satisfied { .. })
                {
                    let destinations_ok = destinations_match
                        || (!cached.used_subgraphs.is_empty() && {
                            let any_conflict = excluded_destinations
                                .newly_excluded_in(&cached.excluded_destinations)
                                .any(|dest| cached.used_subgraphs.contains(dest));
                            !any_conflict
                        });

                    if destinations_ok {
                        return ConditionResolutionCacheResult::Hit(cached.resolution.clone());
                    }
                }

                // Unsatisfied monotonicity: if a condition was unsatisfied with fewer
                // exclusions, it remains unsatisfied with a superset of exclusions.
                if &cached.context == context
                    && matches!(&cached.resolution, ConditionResolution::Unsatisfied { .. })
                    && excluded_destinations.is_superset_of(&cached.excluded_destinations)
                    && excluded_conditions.is_superset_of(&cached.excluded_conditions)
                {
                    return ConditionResolutionCacheResult::Hit(cached.resolution.clone());
                }
            }
            ConditionResolutionCacheResult::Miss
        } else {
            ConditionResolutionCacheResult::Miss
        }
    }

    pub(crate) fn insert(
        &mut self,
        edge: EdgeIndex,
        resolution: ConditionResolution,
        context: OpGraphPathContext,
        excluded_destinations: ExcludedDestinations,
        excluded_conditions: ExcludedConditions,
    ) {
        let used_subgraphs = if let ConditionResolution::Satisfied {
            path_tree: Some(ref tree),
            ..
        } = resolution
        {
            let mut subgraphs = IndexSet::default();
            tree.collect_subgraphs(&mut subgraphs);
            subgraphs
        } else {
            Default::default()
        };
        self.edge_states
            .entry(edge)
            .or_default()
            .push(CachedConditionEntry {
                resolution,
                context,
                excluded_destinations,
                excluded_conditions,
                used_subgraphs,
            });
    }
}

/// A query plan resolver for edge conditions that caches the outcome per edge.
// PORT_NOTE: This ports the `cachingConditionResolver` function from JS. In JS version, the
//            function creates a closure capturing the QueryPlanningTraversal/ValidationTraversal
//            instance itself The same would be infeasible to implement in Rust due to the cyclic
//            references. Instead, in Rust, it is implemented as `CachingConditionResolver` and
//            `ConditionResolver` traits that will be implemented by `QueryPlanningTraversal` and
//            `ValidationTraversal` structs.
pub(crate) trait CachingConditionResolver {
    fn query_graph(&self) -> &QueryGraph;

    fn resolve_without_cache(
        &mut self,
        edge: EdgeIndex,
        context: &OpGraphPathContext,
        excluded_destinations: &ExcludedDestinations,
        excluded_conditions: &ExcludedConditions,
        extra_conditions: Option<&SelectionSet>,
    ) -> Result<ConditionResolution, FederationError>;

    fn resolver_cache(&mut self) -> &mut ConditionResolverCache;

    fn resolve_with_cache(
        &mut self,
        edge: EdgeIndex,
        context: &OpGraphPathContext,
        excluded_destinations: &ExcludedDestinations,
        excluded_conditions: &ExcludedConditions,
        extra_conditions: Option<&SelectionSet>,
    ) -> Result<ConditionResolution, FederationError> {
        let cache_result = self.resolver_cache().contains(
            edge,
            context,
            excluded_destinations,
            excluded_conditions,
            extra_conditions,
        );

        if let ConditionResolutionCacheResult::Hit(cached_resolution) = cache_result {
            return Ok(cached_resolution);
        }

        let resolution = self.resolve_without_cache(
            edge,
            context,
            excluded_destinations,
            excluded_conditions,
            extra_conditions,
        )?;
        if cache_result.is_miss() {
            self.resolver_cache().insert(
                edge,
                resolution.clone(),
                context.clone(),
                excluded_destinations.clone(),
                excluded_conditions.clone(),
            );
        }
        Ok(resolution)
    }
}

/// Blanket implementation of `ConditionResolver` for any type that implements
/// `CachingConditionResolver`.
impl<T: CachingConditionResolver> ConditionResolver for T {
    fn resolve(
        &mut self,
        edge: EdgeIndex,
        context: &OpGraphPathContext,
        excluded_destinations: &ExcludedDestinations,
        excluded_conditions: &ExcludedConditions,
        extra_conditions: Option<&SelectionSet>,
    ) -> Result<ConditionResolution, FederationError> {
        // Invariant check: The edge must have conditions.
        let graph = &self.query_graph();
        let edge_data = graph.edge_weight(edge)?;
        assert!(
            edge_data.conditions.is_some() || extra_conditions.is_some(),
            "Should not have been called for edge without conditions"
        );

        self.resolve_with_cache(
            edge,
            context,
            excluded_destinations,
            excluded_conditions,
            extra_conditions,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query_graph::graph_path::operation::OpGraphPathContext;
    //use crate::link::graphql_definition::{OperationConditional, OperationConditionalKind, BooleanOrVariable};

    #[test]
    fn test_condition_resolver_cache() {
        let mut cache = ConditionResolverCache::new();

        let edge1 = EdgeIndex::new(1);
        let empty_context = OpGraphPathContext::default();
        let empty_destinations = ExcludedDestinations::default();
        let empty_conditions = ExcludedConditions::default();

        assert!(
            cache
                .contains(
                    edge1,
                    &empty_context,
                    &empty_destinations,
                    &empty_conditions,
                    None
                )
                .is_miss()
        );

        cache.insert(
            edge1,
            ConditionResolution::unsatisfied_conditions(),
            empty_context.clone(),
            empty_destinations.clone(),
            empty_conditions.clone(),
        );

        assert!(
            cache
                .contains(
                    edge1,
                    &empty_context,
                    &empty_destinations,
                    &empty_conditions,
                    None
                )
                .is_hit(),
        );

        let edge2 = EdgeIndex::new(2);

        assert!(
            cache
                .contains(
                    edge2,
                    &empty_context,
                    &empty_destinations,
                    &empty_conditions,
                    None
                )
                .is_miss()
        );
    }
}
