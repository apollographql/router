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
    ) -> Result<ConditionResolution, FederationError>;
}

#[derive(Debug, Clone)]
pub(crate) enum ConditionResolution {
    Satisfied {
        cost: QueryPlanCost,
        path_tree: Option<Arc<OpPathTree>>,
        context_map: Option<Arc<IndexMap<Name, ContextMapEntry>>>,
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
}

/// Cap on how many distinct `(context, excluded_destinations, excluded_conditions)`
/// resolutions are retained per edge. Without this, a single plan/validation traversal over a
/// large, heavily `@key`/`@skip`/`@include`-conditioned graph can accumulate an unbounded number
/// of entries per edge over the traversal's lifetime. Once the cap is hit, the
/// least-recently-used entry is evicted (see `ConditionResolverCache::contains`, which promotes
/// hits to most-recently-used).
const MAX_CACHED_CONDITIONS_PER_EDGE: usize = 8;

/// Skip caching a `Satisfied` resolution whose `context_map` has more than this many entries.
/// `context_map` is the most memory-expensive part of a cache entry (each entry owns a
/// `SelectionSet` per `@context` argument captured), so retaining an unusually large one for the
/// rest of the traversal isn't worth it. Recomputing on the next lookup is the same fallback
/// every entry used before this cache existed -- this just opts the expensive minority back into
/// that fallback rather than pinning their memory in the cache.
const MAX_CONTEXT_MAP_ENTRIES_TO_CACHE: usize = 8;

/// Caches condition-resolution outcomes per edge for the duration of a single query-planning or
/// satisfiability-validation traversal. Bounded in two ways to avoid unbounded memory growth on
/// graphs with many distinct condition combinations: a per-edge entry-count cap with LRU
/// eviction, and a size-based skip for resolutions too large to be worth caching at all.
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

        let Some(entries) = self.edge_states.get_mut(&edge) else {
            return ConditionResolutionCacheResult::Miss;
        };

        let Some(pos) = entries.iter().position(|cached| {
            &cached.context == context
                && &cached.excluded_destinations == excluded_destinations
                && &cached.excluded_conditions == excluded_conditions
        }) else {
            return ConditionResolutionCacheResult::Miss;
        };

        // Promote the hit to most-recently-used so `insert`'s front-eviction targets the
        // actual least-recently-used entry rather than just the oldest-inserted one.
        let cached = entries.remove(pos);
        let resolution = cached.resolution.clone();
        entries.push(cached);
        ConditionResolutionCacheResult::Hit(resolution)
    }

    pub(crate) fn insert(
        &mut self,
        edge: EdgeIndex,
        resolution: ConditionResolution,
        context: OpGraphPathContext,
        excluded_destinations: ExcludedDestinations,
        excluded_conditions: ExcludedConditions,
    ) {
        if let ConditionResolution::Satisfied {
            context_map: Some(context_map),
            ..
        } = &resolution
            && context_map.len() > MAX_CONTEXT_MAP_ENTRIES_TO_CACHE
        {
            return;
        }

        let entries = self.edge_states.entry(edge).or_default();
        entries.push(CachedConditionEntry {
            resolution,
            context,
            excluded_destinations,
            excluded_conditions,
        });
        if entries.len() > MAX_CACHED_CONDITIONS_PER_EDGE {
            // Front of the Vec is the least-recently-used entry: fresh inserts and
            // `contains`-promoted hits both land at the back.
            entries.remove(0);
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

    /// Builds `count` distinct `ExcludedDestinations` values (one excluded subgraph name each),
    /// so each one is a distinct cache key while `context`/`excluded_conditions` stay fixed.
    fn distinct_excluded_destinations(count: usize) -> Vec<ExcludedDestinations> {
        (0..count)
            .map(|i| {
                let subgraph: Arc<str> = Arc::from(format!("subgraph-{i}"));
                ExcludedDestinations::default().add_excluded(&subgraph)
            })
            .collect()
    }

    #[test]
    fn test_condition_resolver_cache_evicts_least_recently_used_per_edge() {
        let mut cache = ConditionResolverCache::new();
        let edge = EdgeIndex::new(1);
        let empty_context = OpGraphPathContext::default();
        let empty_conditions = ExcludedConditions::default();

        let destinations = distinct_excluded_destinations(MAX_CACHED_CONDITIONS_PER_EDGE + 1);
        for destination in &destinations {
            cache.insert(
                edge,
                ConditionResolution::unsatisfied_conditions(),
                empty_context.clone(),
                destination.clone(),
                empty_conditions.clone(),
            );
        }

        assert!(
            cache
                .contains(
                    edge,
                    &empty_context,
                    &destinations[0],
                    &empty_conditions,
                    None
                )
                .is_miss(),
            "oldest entry should have been evicted once the per-edge cap was exceeded"
        );

        for destination in &destinations[1..] {
            assert!(
                cache
                    .contains(edge, &empty_context, destination, &empty_conditions, None)
                    .is_hit(),
                "entries within the cap should still be cached"
            );
        }
    }

    #[test]
    fn test_condition_resolver_cache_hit_promotes_to_most_recently_used() {
        let mut cache = ConditionResolverCache::new();
        let edge = EdgeIndex::new(1);
        let empty_context = OpGraphPathContext::default();
        let empty_conditions = ExcludedConditions::default();

        let destinations = distinct_excluded_destinations(MAX_CACHED_CONDITIONS_PER_EDGE);
        for destination in &destinations {
            cache.insert(
                edge,
                ConditionResolution::unsatisfied_conditions(),
                empty_context.clone(),
                destination.clone(),
                empty_conditions.clone(),
            );
        }

        // Touch the oldest entry so it becomes most-recently-used instead of the next entry to
        // be evicted.
        assert!(
            cache
                .contains(
                    edge,
                    &empty_context,
                    &destinations[0],
                    &empty_conditions,
                    None
                )
                .is_hit()
        );

        // Inserting one more entry should now evict `destinations[1]` (the actual
        // least-recently-used entry now), not `destinations[0]` (just promoted above).
        let extra = distinct_excluded_destinations(1).remove(0);
        cache.insert(
            edge,
            ConditionResolution::unsatisfied_conditions(),
            empty_context.clone(),
            extra,
            empty_conditions.clone(),
        );

        assert!(
            cache
                .contains(
                    edge,
                    &empty_context,
                    &destinations[0],
                    &empty_conditions,
                    None
                )
                .is_hit(),
            "recently-touched entry should have survived eviction"
        );
        assert!(
            cache
                .contains(
                    edge,
                    &empty_context,
                    &destinations[1],
                    &empty_conditions,
                    None
                )
                .is_miss(),
            "the actual least-recently-used entry should have been evicted instead"
        );
    }

    #[test]
    fn test_condition_resolver_cache_skips_oversized_context_map() {
        use apollo_compiler::Schema;
        use apollo_compiler::name;

        use crate::schema::ValidFederationSchema;
        use crate::schema::position::CompositeTypeDefinitionPosition;
        use crate::schema::position::ObjectTypeDefinitionPosition;

        let schema = Schema::parse_and_validate("type Query { a: Int }", "schema.graphql").unwrap();
        let schema = ValidFederationSchema::new(schema).unwrap();
        let query_type: CompositeTypeDefinitionPosition =
            ObjectTypeDefinitionPosition::new(name!("Query")).into();
        let selection_set = SelectionSet::empty(schema, query_type);
        let argument_type = Node::new(Type::Named(name!("Int")));

        let make_context_map = |len: usize| -> IndexMap<Name, ContextMapEntry> {
            (0..len)
                .map(|i| {
                    let name = Name::new(&format!("ctx{i}")).unwrap();
                    (
                        name.clone(),
                        ContextMapEntry {
                            levels_in_data_path: 0,
                            levels_in_query_path: 0,
                            path_tree: None,
                            selection_set: selection_set.clone(),
                            argument_name: name.clone(),
                            argument_type: argument_type.clone(),
                            context_id: name,
                        },
                    )
                })
                .collect()
        };

        let mut cache = ConditionResolverCache::new();
        let empty_context = OpGraphPathContext::default();
        let empty_destinations = ExcludedDestinations::default();
        let empty_conditions = ExcludedConditions::default();

        let edge_ok = EdgeIndex::new(1);
        cache.insert(
            edge_ok,
            ConditionResolution::Satisfied {
                cost: 0.0,
                path_tree: None,
                context_map: Some(Arc::new(make_context_map(MAX_CONTEXT_MAP_ENTRIES_TO_CACHE))),
            },
            empty_context.clone(),
            empty_destinations.clone(),
            empty_conditions.clone(),
        );
        assert!(
            cache
                .contains(
                    edge_ok,
                    &empty_context,
                    &empty_destinations,
                    &empty_conditions,
                    None
                )
                .is_hit(),
            "a context_map at the size threshold should still be cached"
        );

        let edge_too_big = EdgeIndex::new(2);
        cache.insert(
            edge_too_big,
            ConditionResolution::Satisfied {
                cost: 0.0,
                path_tree: None,
                context_map: Some(Arc::new(make_context_map(
                    MAX_CONTEXT_MAP_ENTRIES_TO_CACHE + 1,
                ))),
            },
            empty_context.clone(),
            empty_destinations.clone(),
            empty_conditions.clone(),
        );
        assert!(
            cache
                .contains(
                    edge_too_big,
                    &empty_context,
                    &empty_destinations,
                    &empty_conditions,
                    None
                )
                .is_miss(),
            "a context_map over the size threshold should not be cached"
        );
    }
}
