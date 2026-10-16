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

/// Build a search space over a supergraph with connectors, indexed the way
/// `QueryPlanner::new` indexes them for the incremental planner.
pub(super) fn connector_search_space(sdl: &str) -> FieldRoutingSearchSpace {
    let supergraph = crate::Supergraph::new_with_router_specs(sdl).expect("supergraph parses");
    let schema = crate::connectors::index::add_connector_entity_keys(&supergraph.schema)
        .expect("connector entity keys");
    let api = supergraph
        .to_api_schema(Default::default())
        .expect("api schema");
    let query_graph = build_federated_query_graph(schema.clone(), api, Some(true), Some(true))
        .expect("query graph");
    let mut connectors_by_subgraph = Vec::new();
    for (name, subgraph_schema) in query_graph.subgraph_schemas() {
        let connectors = crate::connectors::Connector::from_schema(subgraph_schema.schema(), name)
            .expect("connectors parse");
        if !connectors.is_empty() {
            connectors_by_subgraph.push((subgraph_schema, connectors));
        }
    }
    let connector_index =
        crate::connectors::index::ConnectorIndex::from_subgraphs(connectors_by_subgraph)
            .expect("connector index");
    FieldRoutingSearchSpace {
        cached_query_graph: CachedQueryGraph::new(Arc::new(query_graph), Default::default()),
        supergraph_schema: schema,
        inconsistent_abstract_types: Default::default(),
        connector_index: Arc::new(connector_index),
        caches: super::PlannerCaches::new(),
        disabled_subgraphs: Default::default(),
    }
}

fn space_for(schema: ValidFederationSchema, api: ValidFederationSchema) -> FieldRoutingSearchSpace {
    let query_graph =
        build_federated_query_graph(schema.clone(), api, None, None).expect("query graph");
    FieldRoutingSearchSpace {
        cached_query_graph: CachedQueryGraph::new(Arc::new(query_graph), Default::default()),
        supergraph_schema: schema,
        inconsistent_abstract_types: Default::default(),
        connector_index: Default::default(),
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
