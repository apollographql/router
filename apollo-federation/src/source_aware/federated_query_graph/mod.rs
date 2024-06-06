use indexmap::IndexMap;
use indexmap::IndexSet;
use petgraph::graph::DiGraph;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;

use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::operation::SelectionSet;
use crate::schema::position::AbstractFieldDefinitionPosition;
use crate::schema::position::AbstractTypeDefinitionPosition;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::OutputTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::sources::source;
use crate::sources::source::SourceId;

pub(crate) mod builder;
pub(crate) mod graph_path;
pub(crate) mod path_tree;

#[derive(Debug)]
pub struct FederatedQueryGraph {
    graph: DiGraph<Node, Edge>,
    supergraph_types_to_root_nodes: IndexMap<ObjectTypeDefinitionPosition, NodeIndex>,
    supergraph_root_kinds_to_types:
        IndexMap<SchemaRootDefinitionKind, ObjectTypeDefinitionPosition>,
    self_conditions: Vec<SelectionSet>,
    non_trivial_followup_edges: IndexMap<EdgeIndex, IndexSet<EdgeIndex>>,
    source_data: source::federated_query_graph::FederatedQueryGraphs,
}

impl FederatedQueryGraph {
    pub(crate) fn graph(&self) -> &DiGraph<Node, Edge> {
        &self.graph
    }

    pub(crate) fn node_weight(&self, node: NodeIndex) -> Result<&Node, FederationError> {
        self.graph.node_weight(node).ok_or_else(|| {
            SingleFederationError::Internal {
                message: "Node unexpectedly missing".to_owned(),
            }
            .into()
        })
    }

    fn node_weight_mut(&mut self, node: NodeIndex) -> Result<&mut Node, FederationError> {
        self.graph.node_weight_mut(node).ok_or_else(|| {
            SingleFederationError::Internal {
                message: "Node unexpectedly missing".to_owned(),
            }
            .into()
        })
    }

    pub(crate) fn edge_weight(&self, edge: EdgeIndex) -> Result<&Edge, FederationError> {
        self.graph.edge_weight(edge).ok_or_else(|| {
            SingleFederationError::Internal {
                message: "Edge unexpectedly missing".to_owned(),
            }
            .into()
        })
    }

    fn edge_weight_mut(&mut self, edge: EdgeIndex) -> Result<&mut Edge, FederationError> {
        self.graph.edge_weight_mut(edge).ok_or_else(|| {
            SingleFederationError::Internal {
                message: "Edge unexpectedly missing".to_owned(),
            }
            .into()
        })
    }

    pub(crate) fn edge_endpoints(
        &self,
        edge: EdgeIndex,
    ) -> Result<(NodeIndex, NodeIndex), FederationError> {
        self.graph.edge_endpoints(edge).ok_or_else(|| {
            SingleFederationError::Internal {
                message: "Edge unexpectedly missing".to_owned(),
            }
            .into()
        })
    }
}

impl FederatedQueryGraph {
    #[cfg(test)]
    pub(crate) fn with_graph(graph: DiGraph<Node, Edge>) -> Self {
        Self {
            graph,
            supergraph_types_to_root_nodes: IndexMap::new(),
            supergraph_root_kinds_to_types: IndexMap::new(),
            self_conditions: Vec::new(),
            non_trivial_followup_edges: IndexMap::new(),
            source_data: source::federated_query_graph::FederatedQueryGraphs::with_graphs(
                IndexMap::new(),
            ),
        }
    }
}

#[derive(Debug)]
pub(crate) enum Node {
    Root {
        supergraph_type: ObjectTypeDefinitionPosition,
        source_entering_edges: IndexMap<NodeIndex, IndexSet<EdgeIndex>>,
    },
    Abstract {
        supergraph_type: AbstractTypeDefinitionPosition,
        field_edges: IndexMap<AbstractFieldDefinitionPosition, EdgeIndex>,
        concrete_type_condition_edges: IndexMap<ObjectTypeDefinitionPosition, EdgeIndex>,
        abstract_type_condition_edges: IndexMap<AbstractTypeDefinitionPosition, EdgeIndex>,
        source_id: SourceId,
        source_data: source::federated_query_graph::AbstractNode,
    },
    Concrete {
        supergraph_type: ObjectTypeDefinitionPosition,
        field_edges: IndexMap<ObjectFieldDefinitionPosition, EdgeIndex>,
        source_exiting_edge: Option<EdgeIndex>,
        source_id: SourceId,
        source_data: source::federated_query_graph::ConcreteNode,
    },
    Enum {
        supergraph_type: EnumTypeDefinitionPosition,
        source_id: SourceId,
        source_data: source::federated_query_graph::EnumNode,
    },
    Scalar {
        supergraph_type: ScalarTypeDefinitionPosition,
        source_id: SourceId,
        source_data: source::federated_query_graph::ScalarNode,
    },
}

impl Node {
    pub(crate) fn supergraph_type(&self) -> OutputTypeDefinitionPosition {
        match self {
            Node::Root {
                supergraph_type, ..
            } => supergraph_type.clone().into(),
            Node::Abstract {
                supergraph_type, ..
            } => supergraph_type.clone().into(),
            Node::Concrete {
                supergraph_type, ..
            } => supergraph_type.clone().into(),
            Node::Enum {
                supergraph_type, ..
            } => supergraph_type.clone().into(),
            Node::Scalar {
                supergraph_type, ..
            } => supergraph_type.clone().into(),
        }
    }

    pub(crate) fn source_id(&self) -> Option<&SourceId> {
        match self {
            Node::Root { .. } => None,
            Node::Abstract { source_id, .. } => Some(source_id),
            Node::Concrete { source_id, .. } => Some(source_id),
            Node::Enum { source_id, .. } => Some(source_id),
            Node::Scalar { source_id, .. } => Some(source_id),
        }
    }
}

#[derive(Debug)]
pub(crate) enum Edge {
    AbstractField {
        supergraph_field: AbstractFieldDefinitionPosition,
        matches_concrete_options: bool,
        source_id: SourceId,
        source_data: source::federated_query_graph::AbstractFieldEdge,
    },
    ConcreteField {
        supergraph_field: ObjectFieldDefinitionPosition,
        self_conditions: IndexSet<SelfConditionIndex>,
        source_id: SourceId,
        source_data: source::federated_query_graph::ConcreteFieldEdge,
    },
    TypeCondition {
        supergraph_type: CompositeTypeDefinitionPosition,
        source_id: SourceId,
        source_data: source::federated_query_graph::TypeConditionEdge,
    },
    SourceEntering {
        supergraph_type: ObjectTypeDefinitionPosition,
        self_conditions: IndexSet<SelfConditionIndex>,
        tail_source_id: SourceId,
        source_data: source::federated_query_graph::SourceEnteringEdge,
    },
    SourceExiting {
        supergraph_type: ObjectTypeDefinitionPosition,
        head_source_id: SourceId,
    },
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) struct SelfConditionIndex(usize);
