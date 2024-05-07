use std::sync::Arc;

use petgraph::prelude::EdgeIndex;

use crate::error::FederationError;
use crate::source_aware::federated_query_graph::graph_path::ConditionResolutionId;
use crate::source_aware::federated_query_graph::path_tree::FederatedPathTreeChildKey;
use crate::source_aware::federated_query_graph::FederatedQueryGraph;
use crate::source_aware::federated_query_graph::FederatedQueryGraphEdge;
use crate::source_aware::query_plan::FetchDataPathElement;
use crate::source_aware::query_plan::QueryPlanCost;
use crate::sources::connect::ConnectPath;
use crate::sources::SourceFetchDependencyGraphApi;
use crate::sources::SourceFetchDependencyGraphNode;
use crate::sources::SourceFetchNode;
use crate::sources::SourceId;
use crate::sources::SourcePath;

/// A connect-specific dependency graph for fetches.
#[derive(Debug)]
pub(crate) struct ConnectFetchDependencyGraph;

impl SourceFetchDependencyGraphApi for ConnectFetchDependencyGraph {
    fn can_reuse_node<'path_tree>(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _merge_at: &[FetchDataPathElement],
        _source_entering_edge: EdgeIndex,
        _path_tree_edges: Vec<&'path_tree FederatedPathTreeChildKey>,
        _source_data: &SourceFetchDependencyGraphNode,
    ) -> Result<Vec<&'path_tree FederatedPathTreeChildKey>, FederationError> {
        todo!()
    }

    fn add_node<'path_tree>(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _merge_at: Arc<Vec<FetchDataPathElement>>,
        _source_entering_edge: EdgeIndex,
        _self_condition_resolution: Option<ConditionResolutionId>,
        _path_tree_edges: Vec<&'path_tree FederatedPathTreeChildKey>,
    ) -> Result<
        (
            SourceFetchDependencyGraphNode,
            Vec<&'path_tree FederatedPathTreeChildKey>,
        ),
        FederationError,
    > {
        todo!()
    }

    fn new_path(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        merge_at: Arc<Vec<FetchDataPathElement>>,
        source_entering_edge: EdgeIndex,
        _self_condition_resolution: Option<ConditionResolutionId>,
    ) -> Result<SourcePath, FederationError> {
        // Grab the corresponding source for this edge, making sure that the edge is
        // actually a valid entrypoint.
        let edge_source_id = {
            let graph_edge = query_graph.edge_weight(source_entering_edge)?;

            let FederatedQueryGraphEdge::SourceEntering { tail_source_id, .. } = graph_edge else {
                return Err(FederationError::internal(
                    "a path should start from an entering edge",
                ));
            };

            tail_source_id.clone()
        };

        Ok(SourcePath::Connect(ConnectPath {
            merge_at,
            source_entering_edge,
            source_id: edge_source_id,
            field: None,
        }))
    }

    fn add_path(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _source_path: SourcePath,
        _source_data: &mut SourceFetchDependencyGraphNode,
    ) -> Result<(), FederationError> {
        todo!()
    }

    fn to_cost(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _source_id: SourceId,
        _source_data: &SourceFetchDependencyGraphNode,
    ) -> Result<QueryPlanCost, FederationError> {
        todo!()
    }

    fn to_plan_node(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _source_id: SourceId,
        _source_data: &SourceFetchDependencyGraphNode,
        _fetch_count: u32,
    ) -> Result<SourceFetchNode, FederationError> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::ast::Name;
    use indexmap::IndexMap;
    use petgraph::graph::DiGraph;
    use petgraph::prelude::EdgeIndex;

    use crate::schema::position::ObjectFieldDefinitionPosition;
    use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
    use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;
    use crate::schema::position::ObjectTypeDefinitionPosition;
    use crate::source_aware::federated_query_graph::FederatedQueryGraph;
    use crate::source_aware::federated_query_graph::FederatedQueryGraphEdge;
    use crate::source_aware::federated_query_graph::FederatedQueryGraphNode;
    use crate::sources::connect::selection_parser::Property;
    use crate::sources::connect::ConnectFederatedConcreteQueryGraphNode;
    use crate::sources::connect::ConnectId;
    use crate::sources::connect::ConnectPath;
    use crate::sources::SourceFederatedConcreteQueryGraphNode;
    use crate::sources::SourceFetchDependencyGraphApi;
    use crate::sources::SourceId;
    use crate::sources::SourcePath;

    use super::ConnectFetchDependencyGraph;

    struct SetupInfo {
        fetch_graph: ConnectFetchDependencyGraph,
        query_graph: Arc<FederatedQueryGraph>,
        source_id: SourceId,
        source_entry_edges: Vec<EdgeIndex>,
        non_source_entry_edges: Vec<EdgeIndex>,
    }
    fn setup() -> SetupInfo {
        let mut graph = DiGraph::new();

        // Fill in some dummy data
        // Root
        // |- Post (entry edge)
        // |- User (entry edge)
        // |- View
        let source_id = SourceId::Connect(ConnectId {
            label: "test connect".to_string(),
            subgraph_name: "CONNECT".into(),
            directive: ObjectOrInterfaceFieldDirectivePosition {
                field: ObjectOrInterfaceFieldDefinitionPosition::Object(
                    ObjectFieldDefinitionPosition {
                        type_name: Name::new("TestObject").unwrap(),
                        field_name: Name::new("testField").unwrap(),
                    },
                ),
                directive_name: Name::new("connect").unwrap(),
                directive_index: 0,
            },
        });

        // Create a root
        let query = graph.add_node(FederatedQueryGraphNode::Concrete {
            supergraph_type: ObjectTypeDefinitionPosition {
                type_name: Name::new("Query").unwrap(),
            },
            field_edges: IndexMap::new(),
            source_exiting_edge: None,
            source_id: source_id.clone(),
            source_data: SourceFederatedConcreteQueryGraphNode::Connect(
                ConnectFederatedConcreteQueryGraphNode::SelectionRoot {
                    subgraph_type: ObjectTypeDefinitionPosition {
                        type_name: Name::new("Post").unwrap(),
                    },
                    property_path: vec![Property::Field("posts".to_string())],
                },
            ),
        });

        // Make the nodes with entrypoints
        let mut edges = Vec::new();
        let entrypoints = 2;
        for (index, name) in ["Post", "User", "View"].into_iter().enumerate() {
            let node_type = ObjectTypeDefinitionPosition {
                type_name: Name::new(name).unwrap(),
            };

            let node = graph.add_node(FederatedQueryGraphNode::Concrete {
                supergraph_type: node_type.clone(),
                field_edges: IndexMap::new(),
                source_exiting_edge: None,
                source_id: source_id.clone(),
                source_data: SourceFederatedConcreteQueryGraphNode::Connect(
                    ConnectFederatedConcreteQueryGraphNode::SelectionRoot {
                        subgraph_type: node_type.clone(),
                        property_path: vec![Property::Field(name.to_lowercase().to_string())],
                    },
                ),
            });

            edges.push(graph.add_edge(
                query,
                node,
                FederatedQueryGraphEdge::ConcreteField {
                    supergraph_field: ObjectFieldDefinitionPosition {
                        type_name: Name::new(name).unwrap(),
                        field_name: Name::new(name.to_lowercase()).unwrap(),
                    },
                    self_conditions: None,
                    source_id: source_id.clone(),
                    source_data: None,
                },
            ));

            // Optionally add the entrypoint
            if index < entrypoints {
                edges.push(graph.add_edge(
                    query,
                    node,
                    FederatedQueryGraphEdge::SourceEntering {
                        supergraph_type: node_type,
                        self_conditions: None,
                        tail_source_id: source_id.clone(),
                        source_data: None,
                    },
                ));
            }
        }

        let (entry, non_entry) = edges.into_iter().partition(|&edge_index| {
            matches!(
                graph.edge_weight(edge_index),
                Some(FederatedQueryGraphEdge::SourceEntering { .. })
            )
        });
        SetupInfo {
            fetch_graph: ConnectFetchDependencyGraph,
            query_graph: Arc::new(FederatedQueryGraph::with_graph(graph)),
            source_id,
            source_entry_edges: entry,
            non_source_entry_edges: non_entry,
        }
    }

    #[test]
    fn it_handles_a_new_path() {
        let SetupInfo {
            fetch_graph,
            query_graph,
            source_entry_edges,
            source_id,
            ..
        } = setup();

        let path = fetch_graph
            .new_path(
                query_graph,
                Arc::new(Vec::new()),
                source_entry_edges[0],
                None,
            )
            .unwrap();

        assert_eq!(
            path,
            SourcePath::Connect(ConnectPath {
                merge_at: Arc::new(Vec::new()),
                source_entering_edge: source_entry_edges[0],
                source_id,
                field: None
            })
        );
    }

    #[test]
    fn it_fails_with_invalid_entrypoint() {}

    #[test]
    fn it_fails_with_malformed_graph() {}
}
