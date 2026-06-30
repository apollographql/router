use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Type;
use apollo_compiler::collections::IndexMap;
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
        cache: &mut ConditionResolverCache,
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
    excluded_destinations: ExcludedDestinations,
    excluded_conditions: ExcludedConditions,
}

pub(crate) struct ConditionResolverCache {
    // For every edge having a condition, we cache the resolution of its conditions when possible.
    // Each edge may have multiple cached entries, one per distinct set of excluded destinations
    // seen during resolution. Excluded destinations affect the resolution by preventing key-jump
    // cycles (e.g. A→B→A). Since the algorithm always tries keys in the same order (the order
    // of edges in the query graph), we can store and look up entries by exact match on the
    // excluded destinations.
    //
    // The cache is shared across recursion depths so that results discovered at any depth are
    // visible to all other depths within the same query plan.
    edge_states: IndexMap<EdgeIndex, Vec<CachedConditionEntry>>,
}

impl ConditionResolverCache {
    pub(crate) fn new() -> Self {
        Self {
            edge_states: Default::default(),
        }
    }

    pub(crate) fn contains(
        &self,
        edge: EdgeIndex,
        context: &OpGraphPathContext,
        excluded_destinations: &ExcludedDestinations,
        excluded_conditions: &ExcludedConditions,
        extra_conditions: Option<&SelectionSet>,
    ) -> ConditionResolutionCacheResult {
        // We don't cache when there are extra conditions or a non-empty context. Extra conditions
        // come from edges whose conditions are supplied externally rather than from the edge weight,
        // and the context carries @include/@skip state that would need to be threaded into the
        // resolution's path tree. Caching per-context is possible but not yet implemented.
        // TODO: we could cache with an empty context and then apply the proper transformation on
        // the cached value's `pathTree` when the context is not empty. The context would need to be
        // added to the trigger of key edges in the resolution path tree when appropriate. The
        // context is about active @include/@skip and it's not used that commonly, so this is
        // probably not an urgent improvement.
        if extra_conditions.is_some() || !context.is_empty() {
            return ConditionResolutionCacheResult::NotApplicable;
        }

        if let Some(entries) = self.edge_states.get(&edge) {
            for cached in entries {
                if &cached.excluded_destinations == excluded_destinations
                    && &cached.excluded_conditions == excluded_conditions
                {
                    return ConditionResolutionCacheResult::Hit(cached.resolution.clone());
                }
            }
            // Edge has cached entries but none matched
            ConditionResolutionCacheResult::NotApplicable
        } else {
            ConditionResolutionCacheResult::Miss
        }
    }

    pub(crate) fn insert(
        &mut self,
        edge: EdgeIndex,
        resolution: ConditionResolution,
        excluded_destinations: ExcludedDestinations,
        excluded_conditions: ExcludedConditions,
    ) {
        let entries = self.edge_states.entry(edge).or_default();
        let already_exists = entries.iter().any(|e| {
            e.excluded_destinations == excluded_destinations
                && e.excluded_conditions == excluded_conditions
        });
        if !already_exists {
            entries.push(CachedConditionEntry {
                resolution,
                excluded_destinations,
                excluded_conditions,
            });
        }
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
        &self,
        edge: EdgeIndex,
        context: &OpGraphPathContext,
        excluded_destinations: &ExcludedDestinations,
        excluded_conditions: &ExcludedConditions,
        extra_conditions: Option<&SelectionSet>,
        cache: &mut ConditionResolverCache,
    ) -> Result<ConditionResolution, FederationError>;

    fn resolve_with_cache(
        &self,
        edge: EdgeIndex,
        context: &OpGraphPathContext,
        excluded_destinations: &ExcludedDestinations,
        excluded_conditions: &ExcludedConditions,
        extra_conditions: Option<&SelectionSet>,
        cache: &mut ConditionResolverCache,
    ) -> Result<ConditionResolution, FederationError> {
        let cache_result = cache.contains(
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
            cache,
        )?;
        // See if this resolution is eligible to be inserted into the cache.
        cache.insert(
            edge,
            resolution.clone(),
            excluded_destinations.clone(),
            excluded_conditions.clone(),
        );
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
        cache: &mut ConditionResolverCache,
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
            cache,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query_graph::graph_path::operation::OpGraphPathContext;

    fn dest(names: &[&str]) -> ExcludedDestinations {
        ExcludedDestinations::from_names(names)
    }

    fn cond(ids: &[usize]) -> ExcludedConditions {
        let schema = crate::schema::ValidFederationSchema::new(
            apollo_compiler::Schema::parse_and_validate("type Query { _: ID }", "test.graphql")
                .unwrap(),
        )
        .unwrap();
        let mut result = ExcludedConditions::default();
        for id in ids {
            let type_name = Name::new_unchecked(&format!("C{id}"));
            let ss = SelectionSet::empty(
                schema.clone(),
                crate::schema::position::CompositeTypeDefinitionPosition::Object(
                    crate::schema::position::ObjectTypeDefinitionPosition { type_name },
                ),
            );
            result = result.add_item(&ss);
        }
        result
    }

    fn ctx() -> OpGraphPathContext {
        OpGraphPathContext::default()
    }

    #[test]
    fn exact_match_returns_hit() {
        let mut cache = ConditionResolverCache::new();
        let edge = EdgeIndex::new(1);
        let d = dest(&["subA"]);
        let c = cond(&[]);

        assert!(cache.contains(edge, &ctx(), &d, &c, None).is_miss());
        cache.insert(
            edge,
            ConditionResolution::unsatisfied_conditions(),
            d.clone(),
            c.clone(),
        );
        assert!(cache.contains(edge, &ctx(), &d, &c, None).is_hit());
    }

    #[test]
    fn miss_on_unknown_edge() {
        let mut cache = ConditionResolverCache::new();
        let c = cond(&[]);
        let d = dest(&[]);
        cache.insert(
            EdgeIndex::new(1),
            ConditionResolution::unsatisfied_conditions(),
            d.clone(),
            c.clone(),
        );
        assert!(
            cache
                .contains(EdgeIndex::new(2), &ctx(), &d, &c, None)
                .is_miss()
        );
    }

    #[test]
    fn multi_entry_per_edge() {
        let mut cache = ConditionResolverCache::new();
        let edge = EdgeIndex::new(1);
        let c = cond(&[]);
        cache.insert(
            edge,
            ConditionResolution::unsatisfied_conditions(),
            dest(&["subA"]),
            c.clone(),
        );
        cache.insert(
            edge,
            ConditionResolution::no_conditions(),
            dest(&["subB"]),
            c.clone(),
        );
        assert!(
            cache
                .contains(edge, &ctx(), &dest(&["subA"]), &c, None)
                .is_hit()
        );
        assert!(
            cache
                .contains(edge, &ctx(), &dest(&["subB"]), &c, None)
                .is_hit()
        );
    }

    #[test]
    fn dedup_prevents_duplicate_entries() {
        let mut cache = ConditionResolverCache::new();
        let edge = EdgeIndex::new(1);
        let d = dest(&["subA"]);
        let c = cond(&[]);
        cache.insert(
            edge,
            ConditionResolution::unsatisfied_conditions(),
            d.clone(),
            c.clone(),
        );
        cache.insert(
            edge,
            ConditionResolution::unsatisfied_conditions(),
            d.clone(),
            c.clone(),
        );
        assert_eq!(cache.edge_states.get(&edge).unwrap().len(), 1);
    }
}
