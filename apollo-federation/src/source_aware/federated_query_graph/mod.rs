use indexmap::IndexMap;
use indexmap::IndexSet;
use petgraph::graph::DiGraph;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;

use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::query_plan::operation::SelectionSet;
use crate::schema::position::AbstractFieldDefinitionPosition;
use crate::schema::position::AbstractTypeDefinitionPosition;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::OutputTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::sources::SourceFederatedAbstractFieldQueryGraphEdge;
use crate::sources::SourceFederatedAbstractQueryGraphNode;
use crate::sources::SourceFederatedConcreteFieldQueryGraphEdge;
use crate::sources::SourceFederatedConcreteQueryGraphNode;
use crate::sources::SourceFederatedEnumQueryGraphNode;
use crate::sources::SourceFederatedQueryGraphs;
use crate::sources::SourceFederatedScalarQueryGraphNode;
use crate::sources::SourceFederatedSourceEnteringQueryGraphEdge;
use crate::sources::SourceFederatedTypeConditionQueryGraphEdge;
use crate::sources::SourceId;

pub(crate) mod builder;
pub(crate) mod graph_path;
pub(crate) mod path_tree;

#[derive(Debug)]
pub struct FederatedQueryGraph {
    pub(crate) graph: DiGraph<FederatedQueryGraphNode, FederatedQueryGraphEdge>,
    supergraph_types_to_root_nodes: IndexMap<ObjectTypeDefinitionPosition, NodeIndex>,
    supergraph_root_kinds_to_types:
        IndexMap<SchemaRootDefinitionKind, ObjectTypeDefinitionPosition>,
    self_conditions: Vec<SelectionSet>,
    non_trivial_followup_edges: IndexMap<EdgeIndex, IndexSet<EdgeIndex>>,
    source_data: SourceFederatedQueryGraphs,
}

impl FederatedQueryGraph {
    #[cfg(test)]
    pub(crate) fn with_graph(
        graph: DiGraph<FederatedQueryGraphNode, FederatedQueryGraphEdge>,
    ) -> Self {
        Self {
            graph,
            supergraph_types_to_root_nodes: todo!(),
            supergraph_root_kinds_to_types: todo!(),
            self_conditions: todo!(),
            non_trivial_followup_edges: todo!(),
            source_data: todo!(),
        }
    }
}

#[derive(Debug)]
pub(crate) enum FederatedQueryGraphNode {
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
        source_data: SourceFederatedAbstractQueryGraphNode,
    },
    Concrete {
        supergraph_type: ObjectTypeDefinitionPosition,
        field_edges: IndexMap<ObjectFieldDefinitionPosition, EdgeIndex>,
        source_exiting_edge: Option<EdgeIndex>,
        source_id: SourceId,
        source_data: SourceFederatedConcreteQueryGraphNode,
    },
    Enum {
        supergraph_type: EnumTypeDefinitionPosition,
        source_id: SourceId,
        source_data: SourceFederatedEnumQueryGraphNode,
    },
    Scalar {
        supergraph_type: ScalarTypeDefinitionPosition,
        source_id: SourceId,
        source_data: SourceFederatedScalarQueryGraphNode,
    },
}

impl FederatedQueryGraphNode {
    pub(crate) fn supergraph_type(&self) -> OutputTypeDefinitionPosition {
        match self {
            FederatedQueryGraphNode::Root {
                supergraph_type, ..
            } => supergraph_type.clone().into(),
            FederatedQueryGraphNode::Abstract {
                supergraph_type, ..
            } => supergraph_type.clone().into(),
            FederatedQueryGraphNode::Concrete {
                supergraph_type, ..
            } => supergraph_type.clone().into(),
            FederatedQueryGraphNode::Enum {
                supergraph_type, ..
            } => supergraph_type.clone().into(),
            FederatedQueryGraphNode::Scalar {
                supergraph_type, ..
            } => supergraph_type.clone().into(),
        }
    }

    pub(crate) fn source_id(&self) -> Option<&SourceId> {
        match self {
            FederatedQueryGraphNode::Root { .. } => None,
            FederatedQueryGraphNode::Abstract { source_id, .. } => Some(source_id),
            FederatedQueryGraphNode::Concrete { source_id, .. } => Some(source_id),
            FederatedQueryGraphNode::Enum { source_id, .. } => Some(source_id),
            FederatedQueryGraphNode::Scalar { source_id, .. } => Some(source_id),
        }
    }
}

#[derive(Debug)]
pub(crate) enum FederatedQueryGraphEdge {
    AbstractField {
        supergraph_field: AbstractFieldDefinitionPosition,
        matches_concrete_options: bool,
        source_id: SourceId,
        source_data: Option<SourceFederatedAbstractFieldQueryGraphEdge>,
    },
    ConcreteField {
        supergraph_field: ObjectFieldDefinitionPosition,
        self_conditions: Option<SelfConditionIndex>,
        source_id: SourceId,
        source_data: Option<SourceFederatedConcreteFieldQueryGraphEdge>,
    },
    TypeCondition {
        supergraph_type: CompositeTypeDefinitionPosition,
        source_id: SourceId,
        source_data: Option<SourceFederatedTypeConditionQueryGraphEdge>,
    },
    SourceEntering {
        supergraph_type: ObjectTypeDefinitionPosition,
        self_conditions: Option<SelfConditionIndex>,
        tail_source_id: SourceId,
        source_data: Option<SourceFederatedSourceEnteringQueryGraphEdge>,
    },
    SourceExiting {
        supergraph_type: ObjectTypeDefinitionPosition,
        head_source_id: SourceId,
    },
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) struct SelfConditionIndex(usize);

impl FederatedQueryGraph {
    pub(crate) fn graph(&self) -> &DiGraph<FederatedQueryGraphNode, FederatedQueryGraphEdge> {
        &self.graph
    }

    pub(crate) fn node_weight(
        &self,
        node: NodeIndex,
    ) -> Result<&FederatedQueryGraphNode, FederationError> {
        self.graph.node_weight(node).ok_or_else(|| {
            SingleFederationError::Internal {
                message: "Node unexpectedly missing".to_owned(),
            }
            .into()
        })
    }

    fn node_weight_mut(
        &mut self,
        node: NodeIndex,
    ) -> Result<&mut FederatedQueryGraphNode, FederationError> {
        self.graph.node_weight_mut(node).ok_or_else(|| {
            SingleFederationError::Internal {
                message: "Node unexpectedly missing".to_owned(),
            }
            .into()
        })
    }

    pub(crate) fn edge_weight(
        &self,
        edge: EdgeIndex,
    ) -> Result<&FederatedQueryGraphEdge, FederationError> {
        self.graph.edge_weight(edge).ok_or_else(|| {
            SingleFederationError::Internal {
                message: "Edge unexpectedly missing".to_owned(),
            }
            .into()
        })
    }

    fn edge_weight_mut(
        &mut self,
        edge: EdgeIndex,
    ) -> Result<&mut FederatedQueryGraphEdge, FederationError> {
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
