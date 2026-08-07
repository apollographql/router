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
    /// Estimated heap footprint of this entry (see `estimate_entry_bytes`), computed once at
    /// insert time and cached here so eviction scans don't need to recompute it.
    estimated_bytes: usize,
    /// Logical timestamp of this entry's most recent use (insert or cache hit). Used to find the
    /// globally least-recently-used entry across *all* edges when the cache needs to evict to
    /// stay under `MAX_CACHE_BYTES`.
    last_used: u64,
}

/// Rough per-entry overhead: the `ConditionResolution` enum itself, the `Arc`s and `Vec`/
/// `IndexMap` headers it holds, plus `OpGraphPathContext`/`ExcludedDestinations`/
/// `ExcludedConditions` (all small and `Arc`-backed). Not exact -- an exact figure would mean
/// walking the full AST types involved (`SelectionSet`, `OpPathTree`), which nothing in this
/// crate's dependencies provides a cheap way to do. This is a deliberate approximation, good
/// enough to make eviction decisions that lean on the real skew between entries (a few
/// `context_map`-heavy entries costing much more than many empty ones) rather than treating every
/// entry as equal-cost.
const BASE_ENTRY_OVERHEAD_BYTES: usize = 256;

/// Rough cost of one `ContextMapEntry` inside a `context_map`: a `Name`, a `Node<Type>`, an
/// `Option<Arc<OpPathTree>>` (cheap, `Arc`-backed), and a `SelectionSet` (not `Arc`-backed --
/// the actual variable-cost part, a handful of selections/fields/arguments each with their own
/// heap allocations). This is the dominant term for any resolution that captured
/// `@context`/`@fromContext` state.
const BYTES_PER_CONTEXT_MAP_ENTRY: usize = 512;

/// Total byte budget for the *whole* `ConditionResolverCache` (summed across every edge), not per
/// edge. A per-edge-only bound doesn't help on a graph with a large number of distinct edges each
/// holding just a few entries -- an earlier per-edge-cap-only version of this cache was measured
/// against Wayfair's real production traffic (a 216-subgraph graph) and made no measurable
/// difference to whole-pod memory, because the accumulation there is spread across many edges,
/// not concentrated on a handful of them. This budget is a starting point, not a value derived
/// from profiling the real entry-size distribution -- tune it (or replace the heuristic above
/// with real measurements) once profiling data is available.
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024; // 8 MiB

/// See the constants above for the reasoning behind each term.
fn estimate_entry_bytes(resolution: &ConditionResolution) -> usize {
    let mut size = BASE_ENTRY_OVERHEAD_BYTES;
    if let ConditionResolution::Satisfied {
        context_map: Some(context_map),
        ..
    } = resolution
    {
        size += context_map.len() * BYTES_PER_CONTEXT_MAP_ENTRY;
    }
    size
}

/// Caches condition-resolution outcomes per edge for the duration of a single query-planning or
/// satisfiability-validation traversal. Bounded by a total byte budget (`MAX_CACHE_BYTES`) across
/// the whole cache, not per edge: entries are evicted globally-least-recently-used-first once the
/// budget is exceeded, and any single resolution too large to ever fit the budget is never cached
/// at all (always recomputed, matching the fallback every entry used before this cache existed).
pub(crate) struct ConditionResolverCache {
    edge_states: IndexMap<EdgeIndex, Vec<CachedConditionEntry>>,
    total_bytes: usize,
    next_seq: u64,
    max_bytes: usize,
}

impl ConditionResolverCache {
    pub(crate) fn new() -> Self {
        Self::with_budget(MAX_CACHE_BYTES)
    }

    /// Also used by tests to exercise eviction with a small, easy-to-reason-about budget instead
    /// of needing tens of thousands of inserts to fill the real `MAX_CACHE_BYTES`.
    fn with_budget(max_bytes: usize) -> Self {
        Self {
            edge_states: Default::default(),
            total_bytes: 0,
            next_seq: 0,
            max_bytes,
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

        let Some(cached) = entries.iter_mut().find(|cached| {
            &cached.context == context
                && &cached.excluded_destinations == excluded_destinations
                && &cached.excluded_conditions == excluded_conditions
        }) else {
            return ConditionResolutionCacheResult::Miss;
        };

        self.next_seq += 1;
        cached.last_used = self.next_seq;
        ConditionResolutionCacheResult::Hit(cached.resolution.clone())
    }

    pub(crate) fn insert(
        &mut self,
        edge: EdgeIndex,
        resolution: ConditionResolution,
        context: OpGraphPathContext,
        excluded_destinations: ExcludedDestinations,
        excluded_conditions: ExcludedConditions,
    ) {
        let estimated_bytes = estimate_entry_bytes(&resolution);
        if estimated_bytes > self.max_bytes {
            // This single resolution alone would exceed the whole cache's budget -- never worth
            // caching, always recompute instead.
            return;
        }

        while self.total_bytes + estimated_bytes > self.max_bytes && self.total_bytes > 0 {
            if !self.evict_least_recently_used() {
                break;
            }
        }

        self.next_seq += 1;
        let entries = self.edge_states.entry(edge).or_default();
        entries.push(CachedConditionEntry {
            resolution,
            context,
            excluded_destinations,
            excluded_conditions,
            estimated_bytes,
            last_used: self.next_seq,
        });
        self.total_bytes += estimated_bytes;
    }

    /// Evicts the single globally least-recently-used entry across all edges. Returns `false` if
    /// the cache is already empty. This is an `O(total entries)` scan -- acceptable because
    /// eviction only runs while actually over budget, which keeps the cache's total entry count
    /// bounded by `MAX_CACHE_BYTES` divided by a typical entry's size, not by traversal size.
    fn evict_least_recently_used(&mut self) -> bool {
        let mut oldest: Option<(EdgeIndex, usize, u64)> = None;
        for (edge, entries) in self.edge_states.iter() {
            for (idx, entry) in entries.iter().enumerate() {
                if oldest.is_none_or(|(_, _, last_used)| entry.last_used < last_used) {
                    oldest = Some((*edge, idx, entry.last_used));
                }
            }
        }

        let Some((edge, idx, _)) = oldest else {
            return false;
        };

        let entries = self.edge_states.get_mut(&edge).expect("edge must exist");
        let removed = entries.remove(idx);
        self.total_bytes -= removed.estimated_bytes;
        if entries.is_empty() {
            self.edge_states.shift_remove(&edge);
        }
        true
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

    /// Inserts a plain `unsatisfied_conditions()` entry (no `context_map`, so its estimated size
    /// is always exactly `BASE_ENTRY_OVERHEAD_BYTES`) on the given edge, with fixed/empty
    /// `context`/`excluded_destinations`/`excluded_conditions` -- since each call uses a distinct
    /// `EdgeIndex`, that alone is enough to make each insert a distinct cache entry.
    fn insert_fixed_size_entry(cache: &mut ConditionResolverCache, edge: EdgeIndex) {
        cache.insert(
            edge,
            ConditionResolution::unsatisfied_conditions(),
            OpGraphPathContext::default(),
            ExcludedDestinations::default(),
            ExcludedConditions::default(),
        );
    }

    fn contains_fixed_size_entry(
        cache: &mut ConditionResolverCache,
        edge: EdgeIndex,
    ) -> ConditionResolutionCacheResult {
        cache.contains(
            edge,
            &OpGraphPathContext::default(),
            &ExcludedDestinations::default(),
            &ExcludedConditions::default(),
            None,
        )
    }

    #[test]
    fn test_condition_resolver_cache_evicts_globally_across_edges() {
        // Budget for exactly 3 base-sized (256-byte) entries.
        let mut cache = ConditionResolverCache::with_budget(3 * BASE_ENTRY_OVERHEAD_BYTES);
        let edges: Vec<EdgeIndex> = (0..4).map(EdgeIndex::new).collect();

        for &edge in &edges {
            insert_fixed_size_entry(&mut cache, edge);
        }

        // The 4th insert (on its own, distinct edge) pushed the cache over budget, so the
        // globally-oldest entry -- edge 0 -- should have been evicted, even though the newest
        // insert landed on a completely different edge. A per-edge-only bound would never have
        // caught this, since edge 0 only ever held a single entry.
        assert!(
            contains_fixed_size_entry(&mut cache, edges[0]).is_miss(),
            "oldest entry should be evicted globally once the total budget is exceeded, \
             regardless of which edge it's on"
        );
        for &edge in &edges[1..] {
            assert!(
                contains_fixed_size_entry(&mut cache, edge).is_hit(),
                "entries within the budget should still be cached"
            );
        }
    }

    #[test]
    fn test_condition_resolver_cache_does_not_evict_when_under_budget() {
        let mut cache = ConditionResolverCache::with_budget(3 * BASE_ENTRY_OVERHEAD_BYTES);
        let edges: Vec<EdgeIndex> = (0..3).map(EdgeIndex::new).collect();

        for &edge in &edges {
            insert_fixed_size_entry(&mut cache, edge);
        }

        for &edge in &edges {
            assert!(
                contains_fixed_size_entry(&mut cache, edge).is_hit(),
                "nothing should be evicted while the cache is exactly at (not over) budget"
            );
        }
    }

    #[test]
    fn test_condition_resolver_cache_hit_promotes_to_most_recently_used() {
        let mut cache = ConditionResolverCache::with_budget(3 * BASE_ENTRY_OVERHEAD_BYTES);
        let edges: Vec<EdgeIndex> = (0..3).map(EdgeIndex::new).collect();

        for &edge in &edges {
            insert_fixed_size_entry(&mut cache, edge);
        }

        // Touch edge 0 so it becomes most-recently-used instead of edge 1 being the next in line
        // for eviction.
        assert!(contains_fixed_size_entry(&mut cache, edges[0]).is_hit());

        // Inserting one more entry (on yet another edge) should now evict edge 1 -- the actual
        // least-recently-used entry -- not edge 0, which was just promoted.
        insert_fixed_size_entry(&mut cache, EdgeIndex::new(99));

        assert!(
            contains_fixed_size_entry(&mut cache, edges[0]).is_hit(),
            "recently-touched entry should have survived eviction"
        );
        assert!(
            contains_fixed_size_entry(&mut cache, edges[1]).is_miss(),
            "the actual least-recently-used entry should have been evicted instead"
        );
    }

    /// Builds a `context_map` with `len` entries against a minimal one-field schema, for testing
    /// how cache-entry size scales with `context_map` size.
    fn make_test_context_map(len: usize) -> IndexMap<Name, ContextMapEntry> {
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
    }

    #[test]
    fn test_condition_resolver_cache_skips_entry_exceeding_whole_budget() {
        // 256-byte base overhead + 2*512-byte context_map entries = 1280 bytes, over this budget.
        let mut cache = ConditionResolverCache::with_budget(1000);
        let empty_context = OpGraphPathContext::default();
        let empty_destinations = ExcludedDestinations::default();
        let empty_conditions = ExcludedConditions::default();

        let edge_ok = EdgeIndex::new(1);
        cache.insert(
            edge_ok,
            ConditionResolution::Satisfied {
                cost: 0.0,
                path_tree: None,
                context_map: None,
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
            "a small resolution well under the budget should be cached"
        );

        let edge_too_big = EdgeIndex::new(2);
        cache.insert(
            edge_too_big,
            ConditionResolution::Satisfied {
                cost: 0.0,
                path_tree: None,
                context_map: Some(Arc::new(make_test_context_map(2))),
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
            "a resolution whose estimated size alone exceeds the whole budget should never be \
             cached, and should not have evicted the smaller entry to make room for it"
        );
        // Confirm it really was never inserted, not just immediately evicted -- the small entry
        // above should be untouched.
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
            "the earlier small entry should be unaffected by the oversized insert being rejected"
        );
    }
}
