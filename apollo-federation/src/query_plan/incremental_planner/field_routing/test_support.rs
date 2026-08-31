//! Shared fixtures for field-routing tests.

use std::sync::Arc;

use petgraph::graph::NodeIndex;

use super::FieldRoutingSearchSpace;
use super::cached_query_graph::CachedQueryGraph;
use crate::composition::compose;
use crate::query_graph::build_federated_query_graph;
use crate::schema::ValidFederationSchema;
use crate::subgraph::typestate::Initial;
use crate::subgraph::typestate::Subgraph;

/// Compose the given (name, SDL) subgraphs into a search space.
pub(super) fn search_space(subgraphs: &[(&str, &str)]) -> FieldRoutingSearchSpace {
    let parsed: Vec<Subgraph<Initial>> = subgraphs
        .iter()
        .map(|(name, sdl)| {
            Subgraph::parse(name, &format!("http://{name}"), sdl)
                .unwrap_or_else(|e| panic!("{name} parses: {e}"))
        })
        .collect();
    let supergraph =
        compose(parsed, Default::default()).unwrap_or_else(|e| panic!("composes: {e:?}"));
    let api = supergraph
        .to_api_schema(Default::default())
        .expect("api schema");
    space_for(supergraph.schema().clone(), api)
}

/// Build a search space over an already composed supergraph.
pub(super) fn search_space_from_supergraph(sdl: &str) -> FieldRoutingSearchSpace {
    let supergraph = crate::Supergraph::new(sdl).expect("supergraph parses");
    let api = supergraph
        .to_api_schema(Default::default())
        .expect("api schema");
    space_for(supergraph.schema.clone(), api)
}

fn space_for(schema: ValidFederationSchema, api: ValidFederationSchema) -> FieldRoutingSearchSpace {
    let query_graph =
        build_federated_query_graph(schema.clone(), api, None, None).expect("query graph");
    FieldRoutingSearchSpace {
        cached_query_graph: CachedQueryGraph::new(
            Arc::new(query_graph),
            Default::default(),
            Default::default(),
        ),
        supergraph_schema: schema,
        override_conditions: Default::default(),
        inconsistent_abstract_types: Default::default(),
        caches: super::PlannerCaches::new(),
        disabled_subgraphs: Default::default(),
    }
}

/// The query graph node for `type_name` in `subgraph`.
pub(super) fn node_for(
    space: &FieldRoutingSearchSpace,
    subgraph: &str,
    type_name: &str,
) -> NodeIndex {
    space
        .cached_query_graph
        .query_graph
        .graph()
        .node_indices()
        .find(|&idx| {
            let node = space
                .cached_query_graph
                .query_graph
                .node_weight(idx)
                .expect("node weight exists");
            node.source.as_ref() == subgraph && node.type_.to_string() == type_name
        })
        .unwrap_or_else(|| panic!("{subgraph} has a {type_name} node"))
}
