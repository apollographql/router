use std::sync::Arc;

use apollo_compiler::ast::Name;
use apollo_compiler::ast::Value;
use apollo_compiler::Node as NodeElement;
use indexmap::IndexMap;
use indexmap::IndexSet;
use petgraph::prelude::EdgeIndex;

use crate::error::FederationError;
use crate::source_aware::federated_query_graph;
use crate::source_aware::federated_query_graph::graph_path::ConditionResolutionId;
use crate::source_aware::federated_query_graph::graph_path::OperationPathElement;
use crate::source_aware::federated_query_graph::path_tree;
use crate::source_aware::federated_query_graph::FederatedQueryGraph;
use crate::source_aware::federated_query_graph::SelfConditionIndex;
use crate::source_aware::query_plan::FetchDataPathElement;
use crate::source_aware::query_plan::QueryPlanCost;
use crate::sources::connect::json_selection::PathSelection;
use crate::sources::connect::json_selection::Property;
use crate::sources::connect::Selection;
use crate::sources::connect::SubSelection;
use crate::sources::source;
use crate::sources::source::fetch_dependency_graph::FetchDependencyGraphApi;
use crate::sources::source::fetch_dependency_graph::PathApi;
use crate::sources::source::SourceId;

/// A connect-specific dependency graph for fetches.
#[derive(Debug)]
pub(crate) struct FetchDependencyGraph;

impl FetchDependencyGraphApi for FetchDependencyGraph {
    fn can_reuse_node<'path_tree>(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _merge_at: &[FetchDataPathElement],
        _source_entering_edge: EdgeIndex,
        _path_tree_edges: Vec<&'path_tree path_tree::ChildKey>,
        _source_data: &source::fetch_dependency_graph::Node,
    ) -> Result<Vec<&'path_tree path_tree::ChildKey>, FederationError> {
        todo!()
    }

    fn add_node<'path_tree>(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _merge_at: Arc<[FetchDataPathElement]>,
        _source_entering_edge: EdgeIndex,
        _self_condition_resolution: Option<ConditionResolutionId>,
        _path_tree_edges: Vec<&'path_tree path_tree::ChildKey>,
    ) -> Result<
        (
            source::fetch_dependency_graph::Node,
            Vec<&'path_tree path_tree::ChildKey>,
        ),
        FederationError,
    > {
        todo!()
    }

    fn new_path(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        merge_at: Arc<[FetchDataPathElement]>,
        source_entering_edge: EdgeIndex,
        _self_condition_resolution: Option<ConditionResolutionId>,
    ) -> Result<source::fetch_dependency_graph::Path, FederationError> {
        // Grab the corresponding source for this edge, making sure that the edge is
        // actually a valid entrypoint.
        let edge_source_id = {
            let graph_edge = query_graph.edge_weight(source_entering_edge)?;

            let federated_query_graph::Edge::SourceEntering { tail_source_id, .. } = graph_edge
            else {
                return Err(FederationError::internal(
                    "a path should start from an entering edge",
                ));
            };

            tail_source_id.clone()
        };

        Ok(Path {
            merge_at,
            source_entering_edge,
            source_id: edge_source_id,
            field: None,
        }
        .into())
    }

    fn add_path(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _source_path: source::fetch_dependency_graph::Path,
        _source_data: &mut source::fetch_dependency_graph::Node,
    ) -> Result<(), FederationError> {
        todo!()
    }

    fn to_cost(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _source_id: SourceId,
        _source_data: &source::fetch_dependency_graph::Node,
    ) -> Result<QueryPlanCost, FederationError> {
        todo!()
    }

    fn to_plan_node(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _source_id: SourceId,
        _source_data: &source::fetch_dependency_graph::Node,
        _fetch_count: u32,
    ) -> Result<source::query_plan::FetchNode, FederationError> {
        todo!()
    }
}

#[derive(Debug)]
pub(crate) struct Node {
    merge_at: Arc<[FetchDataPathElement]>,
    source_entering_edge: EdgeIndex,
    field_response_name: Name,
    field_arguments: IndexMap<Name, Value>,
    selection: Selection,
}

#[derive(Debug)]
pub(crate) struct Path {
    merge_at: Arc<[FetchDataPathElement]>,
    source_entering_edge: EdgeIndex,
    source_id: SourceId,
    field: Option<PathField>,
}

#[derive(Debug)]
pub(crate) struct PathField {
    response_name: Name,
    arguments: IndexMap<Name, NodeElement<Value>>,
    selections: PathSelections,
}

#[derive(Debug)]
pub(crate) enum PathSelections {
    Selections {
        head_property_path: Vec<Property>,
        named_selections: Vec<(Name, Vec<Property>)>,
        tail_selection: Option<(Name, PathTailSelection)>,
    },
    CustomScalarRoot {
        selection: Selection,
    },
}

#[derive(Debug)]
pub(crate) enum PathTailSelection {
    Selection {
        property_path: Vec<Property>,
    },
    CustomScalarPathSelection {
        path_selection: PathSelection,
    },
    CustomScalarStarSelection {
        star_subselection: Option<SubSelection>,
        excluded_properties: IndexSet<Property>,
    },
}

impl PathApi for Path {
    fn source_id(&self) -> &SourceId {
        todo!()
    }

    fn add_operation_element(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _operation_element: Arc<OperationPathElement>,
        _edge: Option<EdgeIndex>,
        _self_condition_resolutions: IndexMap<SelfConditionIndex, ConditionResolutionId>,
    ) -> Result<source::fetch_dependency_graph::Path, FederationError> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::ast::Name;
    use apollo_compiler::name;
    use indexmap::IndexMap;
    use insta::assert_debug_snapshot;
    use petgraph::graph::DiGraph;
    use petgraph::prelude::EdgeIndex;

    use super::FetchDependencyGraph;
    use crate::schema::position::ObjectFieldDefinitionPosition;
    use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
    use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;
    use crate::schema::position::ObjectTypeDefinitionPosition;
    use crate::source_aware::federated_query_graph;
    use crate::source_aware::federated_query_graph::FederatedQueryGraph;
    use crate::sources::connect::federated_query_graph::ConcreteFieldEdge;
    use crate::sources::connect::federated_query_graph::ConcreteNode;
    use crate::sources::connect::federated_query_graph::SourceEnteringEdge;
    use crate::sources::connect::json_selection::Property;
    use crate::sources::connect::ConnectId;
    use crate::sources::source::fetch_dependency_graph::FetchDependencyGraphApi;
    use crate::sources::source::SourceId;

    struct SetupInfo {
        fetch_graph: FetchDependencyGraph,
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
                        type_name: name!("TestObject"),
                        field_name: name!("testField"),
                    },
                ),
                directive_name: name!("connect"),
                directive_index: 0,
            },
        });

        // Create a root
        let query = graph.add_node(federated_query_graph::Node::Concrete {
            supergraph_type: ObjectTypeDefinitionPosition {
                type_name: name!("Query"),
            },
            field_edges: IndexMap::new(),
            source_exiting_edge: None,
            source_id: source_id.clone(),
            source_data: ConcreteNode::SelectionRoot {
                subgraph_type: ObjectTypeDefinitionPosition {
                    type_name: name!("Query"),
                },
                property_path: Vec::new(),
            }
            .into(),
        });

        // Make the nodes with entrypoints
        let mut edges = Vec::new();
        let entrypoints = 2;
        for (index, type_name) in ["Post", "User", "View"].into_iter().enumerate() {
            let node_type = ObjectTypeDefinitionPosition {
                type_name: Name::new(type_name).unwrap(),
            };

            let node = graph.add_node(federated_query_graph::Node::Concrete {
                supergraph_type: node_type.clone(),
                field_edges: IndexMap::new(),
                source_exiting_edge: None,
                source_id: source_id.clone(),
                source_data: ConcreteNode::SelectionRoot {
                    subgraph_type: node_type.clone(),
                    property_path: vec![Property::Field(type_name.to_lowercase().to_string())],
                }
                .into(),
            });

            let field = ObjectFieldDefinitionPosition {
                type_name: Name::new(type_name).unwrap(),
                field_name: Name::new(type_name.to_lowercase()).unwrap(),
            };
            edges.push(
                graph.add_edge(
                    query,
                    node,
                    federated_query_graph::Edge::ConcreteField {
                        supergraph_field: field.clone(),
                        self_conditions: None,
                        source_id: source_id.clone(),
                        source_data: ConcreteFieldEdge::Connect {
                            subgraph_field: field.clone(),
                        }
                        .into(),
                    },
                ),
            );

            // Optionally add the entrypoint
            if index < entrypoints {
                edges.push(
                    graph.add_edge(
                        query,
                        node,
                        federated_query_graph::Edge::SourceEntering {
                            supergraph_type: node_type.clone(),
                            self_conditions: None,
                            tail_source_id: source_id.clone(),
                            source_data: SourceEnteringEdge::ConnectParent {
                                subgraph_type: node_type,
                            }
                            .into(),
                        },
                    ),
                );
            }
        }

        let (entry, non_entry) = edges.into_iter().partition(|&edge_index| {
            matches!(
                graph.edge_weight(edge_index),
                Some(federated_query_graph::Edge::SourceEntering { .. })
            )
        });
        SetupInfo {
            fetch_graph: FetchDependencyGraph,
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
            ..
        } = setup();

        // Make sure that the first edge is what we expect
        let last_edge_index = *source_entry_edges.last().unwrap();
        let (query_root_index, post_index) = query_graph.edge_endpoints(last_edge_index).unwrap();
        assert_debug_snapshot!(query_graph.node_weight(query_root_index).unwrap(), @r###"
        Concrete {
            supergraph_type: Object(Query),
            field_edges: {},
            source_exiting_edge: None,
            source_id: Connect(
                ConnectId {
                    label: "test connect",
                    subgraph_name: "CONNECT",
                    directive: ObjectOrInterfaceFieldDirectivePosition {
                        field: Object(TestObject.testField),
                        directive_name: "connect",
                        directive_index: 0,
                    },
                },
            ),
            source_data: Connect(
                SelectionRoot {
                    subgraph_type: Object(Query),
                    property_path: [],
                },
            ),
        }
        "###);
        assert_debug_snapshot!(query_graph.node_weight(post_index).unwrap(), @r###"
        Concrete {
            supergraph_type: Object(User),
            field_edges: {},
            source_exiting_edge: None,
            source_id: Connect(
                ConnectId {
                    label: "test connect",
                    subgraph_name: "CONNECT",
                    directive: ObjectOrInterfaceFieldDirectivePosition {
                        field: Object(TestObject.testField),
                        directive_name: "connect",
                        directive_index: 0,
                    },
                },
            ),
            source_data: Connect(
                SelectionRoot {
                    subgraph_type: Object(User),
                    property_path: [
                        Field(
                            "user",
                        ),
                    ],
                },
            ),
        }
        "###);

        let path = fetch_graph
            .new_path(query_graph, Arc::new([]), last_edge_index, None)
            .unwrap();

        assert_debug_snapshot!(
            path,
            @r###"
        Connect(
            Path {
                merge_at: [],
                source_entering_edge: EdgeIndex(3),
                source_id: Connect(
                    ConnectId {
                        label: "test connect",
                        subgraph_name: "CONNECT",
                        directive: ObjectOrInterfaceFieldDirectivePosition {
                            field: Object(TestObject.testField),
                            directive_name: "connect",
                            directive_index: 0,
                        },
                    },
                ),
                field: None,
            },
        )
        "###
        );
    }

    #[test]
    fn it_fails_with_invalid_entrypoint() {
        let SetupInfo {
            fetch_graph,
            query_graph,
            non_source_entry_edges,
            ..
        } = setup();

        // Make sure that the first edge is what we expect
        let last_edge_index = *non_source_entry_edges.last().unwrap();
        let (query_root_index, view_index) = query_graph.edge_endpoints(last_edge_index).unwrap();
        assert_debug_snapshot!(query_graph.node_weight(query_root_index).unwrap(), @r###"
        Concrete {
            supergraph_type: Object(Query),
            field_edges: {},
            source_exiting_edge: None,
            source_id: Connect(
                ConnectId {
                    label: "test connect",
                    subgraph_name: "CONNECT",
                    directive: ObjectOrInterfaceFieldDirectivePosition {
                        field: Object(TestObject.testField),
                        directive_name: "connect",
                        directive_index: 0,
                    },
                },
            ),
            source_data: Connect(
                SelectionRoot {
                    subgraph_type: Object(Query),
                    property_path: [],
                },
            ),
        }
        "###);
        assert_debug_snapshot!(query_graph.node_weight(view_index).unwrap(), @r###"
        Concrete {
            supergraph_type: Object(View),
            field_edges: {},
            source_exiting_edge: None,
            source_id: Connect(
                ConnectId {
                    label: "test connect",
                    subgraph_name: "CONNECT",
                    directive: ObjectOrInterfaceFieldDirectivePosition {
                        field: Object(TestObject.testField),
                        directive_name: "connect",
                        directive_index: 0,
                    },
                },
            ),
            source_data: Connect(
                SelectionRoot {
                    subgraph_type: Object(View),
                    property_path: [
                        Field(
                            "view",
                        ),
                    ],
                },
            ),
        }
        "###);

        // Make sure that we fail since we do not have an entering edge
        let path = fetch_graph.new_path(query_graph, Arc::new([]), last_edge_index, None);

        assert_debug_snapshot!(
            path,
            @r###"
        Err(
            SingleFederationError(
                Internal {
                    message: "a path should start from an entering edge",
                },
            ),
        )
        "###
        );
    }

    #[test]
    fn it_fails_with_invalid_edge() {
        let SetupInfo {
            fetch_graph,
            query_graph,
            ..
        } = setup();

        // Make sure that the first edge is what we expect
        let invalid_index = EdgeIndex::end();

        // Make sure that we fail since we pass in an invalid edge
        let path = fetch_graph.new_path(query_graph, Arc::new([]), invalid_index, None);

        assert_debug_snapshot!(
            path,
            @r###"
        Err(
            SingleFederationError(
                Internal {
                    message: "Edge unexpectedly missing",
                },
            ),
        )
        "###
        );
    }
}
