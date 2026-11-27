//! `ExcludedConditions` denotes a set: equality and condition-resolver cache lookups must not
//! depend on how many times (or in which order) a condition was added.
use std::collections::BTreeSet;

use super::*;
use crate::Supergraph;
use crate::query_graph::build_query_graph::build_federated_query_graph;
use crate::query_graph::condition_resolver::CachingConditionResolver;
use crate::query_graph::condition_resolver::ConditionResolverCache;

/// A query graph, one edge with conditions, and two distinct selection sets on its head type.
fn fixture() -> (Arc<QueryGraph>, EdgeIndex, [SelectionSet; 2]) {
    let supergraph = Supergraph::new_with_router_specs(include_str!(
        "../../../tests/query_plan/supergraphs/handles_case_of_key_chains_in_parallel_requires.graphql"
    ))
    .unwrap();
    let api = supergraph.to_api_schema(Default::default()).unwrap();
    let graph =
        Arc::new(build_federated_query_graph(supergraph.schema, api, None, Some(true)).unwrap());
    let edge = graph
        .graph
        .edge_indices()
        .find(|e| graph.edge_weight(*e).unwrap().conditions.is_some())
        .unwrap();
    let a = graph
        .edge_weight(edge)
        .unwrap()
        .conditions
        .as_deref()
        .unwrap()
        .clone();
    let b = SelectionSet::parse(a.schema.clone(), a.type_position.clone(), "__typename").unwrap();
    assert_ne!(a, b);
    (graph, edge, [a, b])
}

fn exclusions(selections: &[SelectionSet; 2], indices: &[usize]) -> ExcludedConditions {
    indices
        .iter()
        .fold(ExcludedConditions::default(), |list, i| {
            list.add_item(&selections[*i])
        })
}

/// A reference resolver whose answer is a function of the *set* of excluded conditions (computed
/// independently of `ExcludedConditions`' equality), and which counts uncached resolutions.
struct Resolver {
    graph: Arc<QueryGraph>,
    cache: ConditionResolverCache,
    calls: usize,
}

impl CachingConditionResolver for Resolver {
    fn query_graph(&self) -> &QueryGraph {
        &self.graph
    }
    fn resolver_cache(&mut self) -> &mut ConditionResolverCache {
        &mut self.cache
    }
    fn resolve_without_cache(
        &mut self,
        _edge: EdgeIndex,
        _context: &OpGraphPathContext,
        _destinations: &ExcludedDestinations,
        conditions: &ExcludedConditions,
        _extra: Option<&SelectionSet>,
    ) -> Result<ConditionResolution, FederationError> {
        self.calls += 1;
        let excluded: BTreeSet<_> = conditions.0.iter().map(|s| s.to_string()).collect();
        Ok(ConditionResolution::Satisfied {
            cost: excluded.len() as f64,
            path_tree: None,
            context_map: None,
        })
    }
}

fn resolve(resolver: &mut Resolver, edge: EdgeIndex, conditions: &ExcludedConditions) -> f64 {
    match resolver
        .resolve_with_cache(
            edge,
            &Default::default(),
            &Default::default(),
            conditions,
            None,
        )
        .unwrap()
    {
        ConditionResolution::Satisfied { cost, .. } => cost,
        ConditionResolution::Unsatisfied { .. } => unreachable!(),
    }
}

#[test]
fn exclusion_set_equality_is_symmetric_after_repeated_additions() {
    let (_, _, selections) = fixture();
    let repeated = exclusions(&selections, &[0, 0]);
    let distinct = exclusions(&selections, &[0, 1]);
    assert_eq!(repeated == distinct, distinct == repeated);
    assert_ne!(repeated, distinct);
    // Order and repeats don't matter for equal sets.
    assert_eq!(distinct, exclusions(&selections, &[1, 0, 1]));
    assert_eq!(exclusions(&selections, &[1, 0, 1]), distinct);
}

#[test]
fn repeated_condition_exclusions_do_not_hit_cache_for_a_different_set() {
    let (graph, edge, selections) = fixture();
    let mut resolver = Resolver {
        graph,
        cache: ConditionResolverCache::new(),
        calls: 0,
    };
    // Warmed under {a} (spelled [a, a]), a request for {a, b} must not reuse that entry.
    assert_eq!(
        resolve(&mut resolver, edge, &exclusions(&selections, &[0, 0])),
        1.0
    );
    assert_eq!(
        resolve(&mut resolver, edge, &exclusions(&selections, &[0, 1])),
        2.0
    );
}

#[test]
fn equal_exclusion_sets_share_one_cache_entry() {
    let (graph, edge, selections) = fixture();
    let mut resolver = Resolver {
        graph,
        cache: ConditionResolverCache::new(),
        calls: 0,
    };
    for (spelling, expected) in [
        (&[0, 1][..], 2.0),
        (&[1, 0, 1], 2.0),
        (&[0, 0, 1], 2.0),
        (&[1, 1], 1.0),
        (&[1], 1.0),
    ] {
        assert_eq!(
            resolve(&mut resolver, edge, &exclusions(&selections, spelling)),
            expected
        );
    }
    // Guards against "fixing" transparency by not caching: {a, b} and {b} each resolve once.
    assert_eq!(resolver.calls, 2);
}
