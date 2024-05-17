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
use crate::sources::connect;
use crate::sources::connect::json_selection::JSONSelection;
use crate::sources::connect::json_selection::Key;
use crate::sources::connect::json_selection::PathSelection;
use crate::sources::connect::json_selection::SubSelection;
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
    field_arguments: IndexMap<Name, NodeElement<Value>>,
    selection: Option<JSONSelection>,
}

/// Connect-specific path tracking information.
///
/// A [Path] describes tracking information useful when doing introspection
/// of a connect-specific query.
#[derive(Debug, Clone)]
pub(crate) struct Path {
    merge_at: Arc<[FetchDataPathElement]>,
    source_entering_edge: EdgeIndex,
    source_id: SourceId,
    field: Option<PathField>,
}

#[derive(Debug, Clone)]
pub(crate) struct PathField {
    response_name: Name,
    arguments: IndexMap<Name, NodeElement<Value>>,
    selections: PathSelections,
}

#[derive(Debug, Clone)]
pub(crate) enum PathSelections {
    Selections {
        head_property_path: Vec<Key>,
        named_selections: Vec<(Name, Vec<Key>)>,
        tail_selection: Option<(Name, PathTailSelection)>,
    },
    CustomScalarRoot {
        selection: JSONSelection,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum PathTailSelection {
    Selection {
        property_path: Vec<Key>,
    },
    CustomScalarPathSelection {
        path_selection: PathSelection,
    },
    CustomScalarStarSelection {
        star_subselection: Option<SubSelection>,
        excluded_properties: IndexSet<Key>,
    },
}

impl PathApi for Path {
    fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    fn add_operation_element(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        operation_element: Arc<OperationPathElement>,
        edge: Option<EdgeIndex>,
        _self_condition_resolutions: IndexMap<SelfConditionIndex, ConditionResolutionId>,
    ) -> Result<source::fetch_dependency_graph::Path, FederationError> {
        // For this milestone, we only allow `NormalizedField`s for operation elements
        let OperationPathElement::Field(operation_field) = operation_element.as_ref() else {
            return Err(FederationError::internal(
                "operation elements must be called on a field",
            ));
        };

        // For milestone 1, we don't consider cases where the edge is not present
        let Some(edge) = edge else {
            return Err(FederationError::internal("edge cannot be None"));
        };

        // Extract the edge information for this operation
        let federated_query_graph::Edge::ConcreteField { source_data, .. } =
            query_graph.edge_weight(edge)?
        else {
            return Err(FederationError::internal(
                "operation elements should only be called for concrete fields",
            ));
        };

        let source::federated_query_graph::ConcreteFieldEdge::Connect(concrete_field_edge) =
            source_data
        else {
            return Err(FederationError::internal(
                "operation element's source data must be a connect concrete field",
            ));
        };

        // We need to figure out now what path to take based on what needs updating in the original connect path
        let connect_field = self
            .field
            .to_owned()
            .map(|field| {
                // Deconstruct the original selection
                let PathSelections::Selections {
                    head_property_path,
                    named_selections,
                    tail_selection: None,
                } = field.selections
                else {
                    return Err(FederationError::internal(
                        "expected the existing field to have selections with no tail",
                    ));
                };

                // Recreate it with additional info
                let selections = match concrete_field_edge {
                    connect::federated_query_graph::ConcreteFieldEdge::Selection {
                        property_path,
                        ..
                    } => {
                        let (_, operation_target_index) = query_graph.edge_endpoints(edge)?;
                        let operation_target_node =
                            query_graph.node_weight(operation_target_index)?;

                        match operation_target_node {
                            federated_query_graph::Node::Concrete { .. } => {
                                let concrete_selection = (
                                    operation_field.data().response_name(),
                                    property_path.clone(),
                                );

                                let mut named_selections = named_selections;
                                named_selections.push(concrete_selection);
                                PathSelections::Selections {
                                    head_property_path,
                                    named_selections,
                                    tail_selection: None,
                                }
                            }

                            federated_query_graph::Node::Enum { .. }
                            | federated_query_graph::Node::Scalar { .. } => {
                                let new_tail = PathTailSelection::Selection {
                                    property_path: property_path.clone(),
                                };

                                PathSelections::Selections {
                                    head_property_path,
                                    named_selections,
                                    tail_selection: Some((operation_field.data().response_name(), new_tail)),
                                }
                            }

                            other => return Err(FederationError::internal(format!("expected the tail edge to contain a concrete, enum, or scalar node, found: {other:?}"))),
                        }
                    }

                    connect::federated_query_graph::ConcreteFieldEdge::CustomScalarPathSelection {
                        path_selection,
                        ..
                    } => {
                        let new_tail = PathTailSelection::CustomScalarPathSelection {
                            path_selection: path_selection.clone(),
                        };

                        PathSelections::Selections {
                            head_property_path,
                            named_selections,
                            tail_selection: Some((operation_field.data().response_name(), new_tail)),
                        }
                    }

                    connect::federated_query_graph::ConcreteFieldEdge::CustomScalarStarSelection {
                        star_subselection,
                        excluded_properties,
                        ..
                    } => {
                        let new_tail = PathTailSelection::CustomScalarStarSelection {
                            star_subselection: star_subselection.clone(),
                            excluded_properties: excluded_properties.clone(),
                        };

                        PathSelections::Selections {
                            head_property_path,
                            named_selections,
                            tail_selection: Some((operation_field.data().response_name(), new_tail)),
                        }
                    }

                    _ => {
                        return Err(FederationError::internal(
                            "expected the concrete edge to be a selection",
                        ))
                    }
                };

                Ok(PathField { response_name: field.response_name, arguments: field.arguments, selections })
            })
            .unwrap_or_else(|| {
                let connect::federated_query_graph::ConcreteFieldEdge::Connect {
                    subgraph_field: _subgraph_field,
                } = concrete_field_edge
                else {
                    return Err(FederationError::internal(
                        "expected the field edge to be connect",
                    ));
                };

                let (_, operation_target_index) = query_graph.edge_endpoints(edge)?;
                let operation_target_node = query_graph.node_weight(operation_target_index)?;
                let selections = match operation_target_node {
                    federated_query_graph::Node::Concrete {
                        source_data:
                            source::federated_query_graph::ConcreteNode::Connect(
                                connect::federated_query_graph::ConcreteNode::SelectionRoot {
                                    property_path,
                                    ..
                                },
                            ),
                        ..
                    }
                    | federated_query_graph::Node::Enum {
                        source_data:
                            source::federated_query_graph::EnumNode::Connect(
                                connect::federated_query_graph::EnumNode::SelectionRoot {
                                    property_path,
                                    ..
                                },
                            ),
                        ..
                    }
                    | federated_query_graph::Node::Scalar {
                        source_data:
                            source::federated_query_graph::ScalarNode::Connect(
                                connect::federated_query_graph::ScalarNode::SelectionRoot {
                                    property_path,
                                    ..
                                },
                            ),
                        ..
                    } => PathSelections::Selections {
                        head_property_path: property_path.clone(),
                        named_selections: Vec::new(),
                        tail_selection: None,
                    },

                    federated_query_graph::Node::Scalar {
                        source_data:
                            source::federated_query_graph::ScalarNode::Connect(
                                connect::federated_query_graph::ScalarNode::CustomScalarSelectionRoot {
                                    selection,
                                    ..
                                },
                            ),
                        ..
                    } => PathSelections::CustomScalarRoot {
                        selection: selection.clone(),
                    },

                    _ => {
                        return Err(FederationError::internal(
                            "expected a concrete type, enum, or scalar",
                        ))
                    }
                };

                Ok(PathField {
                    response_name: operation_field.data().response_name(),
                    arguments: operation_field
                        .data()
                        .arguments
                        .iter()
                        .map(|arg| (arg.name.clone(), arg.value.clone()))
                        .collect(),
                    selections,
                })
            })?;

        Ok(source::fetch_dependency_graph::Path::Connect(Path {
            merge_at: self.merge_at.clone(),
            source_entering_edge: self.source_entering_edge,
            source_id: self.source_id.clone(),
            field: Some(connect_field),
        }))
    }
}

#[cfg(test)]
mod tests {
    mod fetch {
        use std::sync::Arc;

        use apollo_compiler::ast::Name;
        use apollo_compiler::name;
        use indexmap::IndexMap;
        use insta::assert_debug_snapshot;
        use insta::assert_snapshot;
        use petgraph::graph::DiGraph;
        use petgraph::prelude::EdgeIndex;

        use crate::schema::position::ObjectFieldDefinitionPosition;
        use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
        use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;
        use crate::schema::position::ObjectTypeDefinitionPosition;
        use crate::source_aware::federated_query_graph;
        use crate::source_aware::federated_query_graph::FederatedQueryGraph;
        use crate::sources::connect::federated_query_graph::ConcreteFieldEdge;
        use crate::sources::connect::federated_query_graph::ConcreteNode;
        use crate::sources::connect::federated_query_graph::SourceEnteringEdge;
        use crate::sources::connect::fetch_dependency_graph::FetchDependencyGraph;
        use crate::sources::connect::json_selection::Key;
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
                        property_path: vec![Key::Field(type_name.to_lowercase().to_string())],
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
            let (query_root_index, post_index) =
                query_graph.edge_endpoints(last_edge_index).unwrap();
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
            let (query_root_index, view_index) =
                query_graph.edge_endpoints(last_edge_index).unwrap();
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

            let Err(path) = path else {
                panic!("Unexpectedly succeeded with non-source-entering edge.")
            };
            assert_snapshot!(
                path,
                @r###"
            An internal error has occurred, please report this bug to Apollo.

            Details: a path should start from an entering edge
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

            let Err(path) = path else {
                panic!("Unexpectedly succeeded with invalid edge.")
            };
            assert_snapshot!(
                path,
                @r###"
            An internal error has occurred, please report this bug to Apollo.

            Details: Edge unexpectedly missing
            "###
            );
        }
    }

    mod path {
        use std::sync::Arc;

        use apollo_compiler::ast::DirectiveList;
        use apollo_compiler::name;
        use apollo_compiler::Schema;
        use indexmap::IndexMap;
        use insta::assert_debug_snapshot;
        use petgraph::graph::DiGraph;
        use petgraph::prelude::EdgeIndex;
        use petgraph::prelude::NodeIndex;

        use crate::query_plan::operation::Field;
        use crate::query_plan::operation::FieldData;
        use crate::schema::position::EnumTypeDefinitionPosition;
        use crate::schema::position::FieldDefinitionPosition;
        use crate::schema::position::ObjectFieldDefinitionPosition;
        use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
        use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;
        use crate::schema::position::ObjectTypeDefinitionPosition;
        use crate::schema::position::ScalarTypeDefinitionPosition;
        use crate::schema::ValidFederationSchema;
        use crate::source_aware::federated_query_graph;
        use crate::source_aware::federated_query_graph::graph_path::OperationPathElement;
        use crate::source_aware::federated_query_graph::FederatedQueryGraph;
        use crate::sources::connect;
        use crate::sources::connect::json_selection::Key;
        use crate::sources::connect::ConnectId;
        use crate::sources::connect::JSONSelection;
        use crate::sources::source;
        use crate::sources::source::fetch_dependency_graph::PathApi;
        use crate::sources::source::SourceId;

        struct SetupInfo {
            graph: DiGraph<federated_query_graph::Node, federated_query_graph::Edge>,
            schema: ValidFederationSchema,
            source_id: SourceId,
        }
        fn setup() -> SetupInfo {
            let mut graph = DiGraph::new();
            let source_id = SourceId::Connect(ConnectId {
                label: "test connect".to_string(),
                subgraph_name: "CONNECT".into(),
                directive: ObjectOrInterfaceFieldDirectivePosition {
                    field: ObjectOrInterfaceFieldDefinitionPosition::Object(
                        ObjectFieldDefinitionPosition {
                            type_name: name!("_testObject"),
                            field_name: name!("testField"),
                        },
                    ),
                    directive_name: name!("connect"),
                    directive_index: 0,
                },
            });

            // Create a dummy schema for tests
            let schema =
                Schema::parse(include_str!("../tests/schemas/simple.graphql"), "").unwrap();
            let schema = schema.validate().unwrap();
            let schema = ValidFederationSchema::new(schema).unwrap();

            // Fill in some dummy data
            graph.add_node(federated_query_graph::Node::Concrete {
                supergraph_type: ObjectTypeDefinitionPosition {
                    type_name: name!("_query"),
                },
                field_edges: IndexMap::new(),
                source_exiting_edge: None,
                source_id: source_id.clone(),
                source_data: source::federated_query_graph::ConcreteNode::Connect(
                    connect::federated_query_graph::ConcreteNode::SelectionRoot {
                        subgraph_type: ObjectTypeDefinitionPosition {
                            type_name: name!("_query"),
                        },
                        property_path: Vec::new(),
                    },
                ),
            });

            SetupInfo {
                graph,
                schema,
                source_id,
            }
        }

        #[test]
        fn it_adds_operation_element_with_no_field_with_concrete() {
            let SetupInfo {
                schema,
                source_id,
                mut graph,
                ..
            } = setup();

            let node_type = ObjectTypeDefinitionPosition {
                type_name: name!("_noFieldConcreteNode"),
            };
            let field_pos = ObjectFieldDefinitionPosition {
                type_name: name!("_noFieldTypeName"),
                field_name: name!("_noFieldFieldName"),
            };
            let node = graph.add_node(federated_query_graph::Node::Concrete {
                supergraph_type: node_type.clone(),
                field_edges: IndexMap::new(),
                source_exiting_edge: None,
                source_id: source_id.clone(),
                source_data: source::federated_query_graph::ConcreteNode::Connect(
                    connect::federated_query_graph::ConcreteNode::SelectionRoot {
                        subgraph_type: node_type.clone(),
                        property_path: vec![Key::Field("no_field_concrete".to_string())],
                    },
                ),
            });

            let edge = graph.add_edge(
                NodeIndex::new(0),
                node,
                federated_query_graph::Edge::ConcreteField {
                    supergraph_field: field_pos.clone(),
                    self_conditions: None,
                    source_id: source_id.clone(),
                    source_data: source::federated_query_graph::ConcreteFieldEdge::Connect(
                        connect::federated_query_graph::ConcreteFieldEdge::Connect {
                            subgraph_field: field_pos.clone(),
                        },
                    ),
                },
            );
            let path = connect::fetch_dependency_graph::Path {
                merge_at: Arc::new([]),
                source_entering_edge: EdgeIndex::end(),
                source_id,
                field: None,
            };
            let operation_element = Arc::new(OperationPathElement::Field(Field::new(FieldData {
                schema,
                field_position: FieldDefinitionPosition::Object(field_pos),
                alias: Some(name!("_noFieldTestAlias")),
                arguments: Arc::new(Vec::new()),
                directives: Arc::new(DirectiveList::new()),
                sibling_typename: None,
            })));

            let result = path
                .add_operation_element(
                    Arc::new(FederatedQueryGraph::with_graph(graph)),
                    operation_element,
                    Some(edge),
                    IndexMap::new(),
                )
                .unwrap();

            assert_debug_snapshot!(result, @r###"
            Connect(
                Path {
                    merge_at: [],
                    source_entering_edge: EdgeIndex(4294967295),
                    source_id: Connect(
                        ConnectId {
                            label: "test connect",
                            subgraph_name: "CONNECT",
                            directive: ObjectOrInterfaceFieldDirectivePosition {
                                field: Object(_testObject.testField),
                                directive_name: "connect",
                                directive_index: 0,
                            },
                        },
                    ),
                    field: Some(
                        PathField {
                            response_name: "_noFieldTestAlias",
                            arguments: {},
                            selections: Selections {
                                head_property_path: [
                                    Field(
                                        "no_field_concrete",
                                    ),
                                ],
                                named_selections: [],
                                tail_selection: None,
                            },
                        },
                    ),
                },
            )
            "###);
        }

        #[test]
        fn it_adds_operation_element_with_no_field_with_enum() {
            let SetupInfo {
                schema,
                source_id,
                mut graph,
                ..
            } = setup();

            let node_type = EnumTypeDefinitionPosition {
                type_name: name!("_noFieldEnumNode"),
            };
            let field_pos = ObjectFieldDefinitionPosition {
                type_name: name!("_noFieldTypeName"),
                field_name: name!("_noFieldFieldName"),
            };
            let node = graph.add_node(federated_query_graph::Node::Enum {
                supergraph_type: node_type.clone(),
                source_id: source_id.clone(),
                source_data: source::federated_query_graph::EnumNode::Connect(
                    connect::federated_query_graph::EnumNode::SelectionRoot {
                        subgraph_type: node_type.clone(),
                        property_path: vec![Key::Field("no_field_enum".to_string())],
                    },
                ),
            });

            let edge = graph.add_edge(
                NodeIndex::new(0),
                node,
                federated_query_graph::Edge::ConcreteField {
                    supergraph_field: field_pos.clone(),
                    self_conditions: None,
                    source_id: source_id.clone(),
                    source_data: source::federated_query_graph::ConcreteFieldEdge::Connect(
                        connect::federated_query_graph::ConcreteFieldEdge::Connect {
                            subgraph_field: field_pos.clone(),
                        },
                    ),
                },
            );
            let path = connect::fetch_dependency_graph::Path {
                merge_at: Arc::new([]),
                source_entering_edge: EdgeIndex::end(),
                source_id,
                field: None,
            };
            let operation_element = Arc::new(OperationPathElement::Field(Field::new(FieldData {
                schema,
                field_position: FieldDefinitionPosition::Object(field_pos),
                alias: Some(name!("_noFieldTestAlias")),
                arguments: Arc::new(Vec::new()),
                directives: Arc::new(DirectiveList::new()),
                sibling_typename: None,
            })));

            let result = path
                .add_operation_element(
                    Arc::new(FederatedQueryGraph::with_graph(graph)),
                    operation_element,
                    Some(edge),
                    IndexMap::new(),
                )
                .unwrap();

            assert_debug_snapshot!(result, @r###"
            Connect(
                Path {
                    merge_at: [],
                    source_entering_edge: EdgeIndex(4294967295),
                    source_id: Connect(
                        ConnectId {
                            label: "test connect",
                            subgraph_name: "CONNECT",
                            directive: ObjectOrInterfaceFieldDirectivePosition {
                                field: Object(_testObject.testField),
                                directive_name: "connect",
                                directive_index: 0,
                            },
                        },
                    ),
                    field: Some(
                        PathField {
                            response_name: "_noFieldTestAlias",
                            arguments: {},
                            selections: Selections {
                                head_property_path: [
                                    Field(
                                        "no_field_enum",
                                    ),
                                ],
                                named_selections: [],
                                tail_selection: None,
                            },
                        },
                    ),
                },
            )
            "###);
        }

        #[test]
        fn it_adds_operation_element_with_no_field_with_custom_scalar() {
            let SetupInfo {
                schema,
                source_id,
                mut graph,
                ..
            } = setup();

            let (_, selection) = JSONSelection::parse(".one.two.three").unwrap();
            let node_type = ScalarTypeDefinitionPosition {
                type_name: name!("_noFieldCustomScalarNode"),
            };
            let field_pos = ObjectFieldDefinitionPosition {
                type_name: name!("_noFieldTypeName"),
                field_name: name!("_noFieldFieldName"),
            };
            let node = graph.add_node(federated_query_graph::Node::Scalar {
                supergraph_type: node_type.clone(),
                source_id: source_id.clone(),
                source_data: source::federated_query_graph::ScalarNode::Connect(
                    connect::federated_query_graph::ScalarNode::CustomScalarSelectionRoot {
                        subgraph_type: node_type.clone(),
                        selection,
                    },
                ),
            });

            let edge = graph.add_edge(
                NodeIndex::new(0),
                node,
                federated_query_graph::Edge::ConcreteField {
                    supergraph_field: field_pos.clone(),
                    self_conditions: None,
                    source_id: source_id.clone(),
                    source_data: source::federated_query_graph::ConcreteFieldEdge::Connect(
                        connect::federated_query_graph::ConcreteFieldEdge::Connect {
                            subgraph_field: field_pos.clone(),
                        },
                    ),
                },
            );
            let path = connect::fetch_dependency_graph::Path {
                merge_at: Arc::new([]),
                source_entering_edge: EdgeIndex::end(),
                source_id,
                field: None,
            };
            let operation_element = Arc::new(OperationPathElement::Field(Field::new(FieldData {
                schema,
                field_position: FieldDefinitionPosition::Object(field_pos),
                alias: Some(name!("_noFieldTestAlias")),
                arguments: Arc::new(Vec::new()),
                directives: Arc::new(DirectiveList::new()),
                sibling_typename: None,
            })));

            let result = path
                .add_operation_element(
                    Arc::new(FederatedQueryGraph::with_graph(graph)),
                    operation_element,
                    Some(edge),
                    IndexMap::new(),
                )
                .unwrap();

            assert_debug_snapshot!(result, @r###"
            Connect(
                Path {
                    merge_at: [],
                    source_entering_edge: EdgeIndex(4294967295),
                    source_id: Connect(
                        ConnectId {
                            label: "test connect",
                            subgraph_name: "CONNECT",
                            directive: ObjectOrInterfaceFieldDirectivePosition {
                                field: Object(_testObject.testField),
                                directive_name: "connect",
                                directive_index: 0,
                            },
                        },
                    ),
                    field: Some(
                        PathField {
                            response_name: "_noFieldTestAlias",
                            arguments: {},
                            selections: CustomScalarRoot {
                                selection: Path(
                                    Key(
                                        Field(
                                            "one",
                                        ),
                                        Key(
                                            Field(
                                                "two",
                                            ),
                                            Key(
                                                Field(
                                                    "three",
                                                ),
                                                Empty,
                                            ),
                                        ),
                                    ),
                                ),
                            },
                        },
                    ),
                },
            )
            "###);
        }

        #[test]
        fn it_adds_operation_element_with_existing_field_with_selection() {
            let SetupInfo {
                schema,
                source_id,
                mut graph,
                ..
            } = setup();

            let node_type = EnumTypeDefinitionPosition {
                type_name: name!("_fieldEnumNode"),
            };
            let field_pos = ObjectFieldDefinitionPosition {
                type_name: name!("_fieldTypeName"),
                field_name: name!("_fieldFieldName"),
            };
            let node = graph.add_node(federated_query_graph::Node::Enum {
                supergraph_type: node_type.clone(),
                source_id: source_id.clone(),
                source_data: source::federated_query_graph::EnumNode::Connect(
                    connect::federated_query_graph::EnumNode::SelectionRoot {
                        subgraph_type: node_type.clone(),
                        property_path: vec![Key::Field("field_enum".to_string())],
                    },
                ),
            });

            let edge = graph.add_edge(
                NodeIndex::new(0),
                node,
                federated_query_graph::Edge::ConcreteField {
                    supergraph_field: field_pos.clone(),
                    self_conditions: None,
                    source_id: source_id.clone(),
                    source_data: source::federated_query_graph::ConcreteFieldEdge::Connect(
                        connect::federated_query_graph::ConcreteFieldEdge::Selection {
                            subgraph_field: field_pos.clone(),
                            property_path: vec!["one", "two", "three"]
                                .into_iter()
                                .map(|prop| Key::Field(prop.to_string()))
                                .collect(),
                        },
                    ),
                },
            );
            let path = connect::fetch_dependency_graph::Path {
                merge_at: Arc::new([]),
                source_entering_edge: EdgeIndex::end(),
                source_id,
                field: Some(connect::fetch_dependency_graph::PathField {
                    response_name: name!("_connectPathResponseName"),
                    arguments: IndexMap::new(),
                    selections: connect::fetch_dependency_graph::PathSelections::Selections {
                        head_property_path: Vec::new(),
                        named_selections: Vec::new(),
                        tail_selection: None,
                    },
                }),
            };
            let operation_element = Arc::new(OperationPathElement::Field(Field::new(FieldData {
                schema,
                field_position: FieldDefinitionPosition::Object(field_pos),
                alias: Some(name!("_fieldTestAlias")),
                arguments: Arc::new(Vec::new()),
                directives: Arc::new(DirectiveList::new()),
                sibling_typename: None,
            })));

            let result = path
                .add_operation_element(
                    Arc::new(FederatedQueryGraph::with_graph(graph)),
                    operation_element,
                    Some(edge),
                    IndexMap::new(),
                )
                .unwrap();

            assert_debug_snapshot!(result, @r###"
            Connect(
                Path {
                    merge_at: [],
                    source_entering_edge: EdgeIndex(4294967295),
                    source_id: Connect(
                        ConnectId {
                            label: "test connect",
                            subgraph_name: "CONNECT",
                            directive: ObjectOrInterfaceFieldDirectivePosition {
                                field: Object(_testObject.testField),
                                directive_name: "connect",
                                directive_index: 0,
                            },
                        },
                    ),
                    field: Some(
                        PathField {
                            response_name: "_connectPathResponseName",
                            arguments: {},
                            selections: Selections {
                                head_property_path: [],
                                named_selections: [],
                                tail_selection: Some(
                                    (
                                        "_fieldTestAlias",
                                        Selection {
                                            property_path: [
                                                Field(
                                                    "one",
                                                ),
                                                Field(
                                                    "two",
                                                ),
                                                Field(
                                                    "three",
                                                ),
                                            ],
                                        },
                                    ),
                                ),
                            },
                        },
                    ),
                },
            )
            "###);
        }
    }
}
