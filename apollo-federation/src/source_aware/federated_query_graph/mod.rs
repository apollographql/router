use crate::query_plan::operation::NormalizedSelectionSet;
use crate::schema::position::{
    AbstractFieldDefinitionPosition, AbstractTypeDefinitionPosition,
    CompositeTypeDefinitionPosition, EnumTypeDefinitionPosition, ObjectFieldDefinitionPosition,
    ObjectTypeDefinitionPosition, OutputTypeDefinitionPosition, ScalarTypeDefinitionPosition,
    SchemaRootDefinitionKind,
};
use crate::sources::{
    SourceFederatedAbstractFieldQueryGraphEdge, SourceFederatedAbstractQueryGraphNode,
    SourceFederatedConcreteFieldQueryGraphEdge, SourceFederatedConcreteQueryGraphNode,
    SourceFederatedEnumQueryGraphNode, SourceFederatedQueryGraphs,
    SourceFederatedScalarQueryGraphNode, SourceFederatedSourceEnteringQueryGraphEdge,
    SourceFederatedTypeConditionQueryGraphEdge, SourceId,
};
use indexmap::{IndexMap, IndexSet};
use petgraph::graph::{DiGraph, EdgeIndex, NodeIndex};

pub(crate) mod builder;
pub(crate) mod graph_path;
pub(crate) mod path_tree;

#[derive(Debug)]
pub struct FederatedQueryGraph {
    graph: DiGraph<FederatedQueryGraphNode, FederatedQueryGraphEdge>,
    supergraph_types_to_root_nodes: IndexMap<ObjectTypeDefinitionPosition, NodeIndex>,
    supergraph_root_kinds_to_types:
        IndexMap<SchemaRootDefinitionKind, ObjectTypeDefinitionPosition>,
    self_conditions: Vec<NormalizedSelectionSet>,
    non_trivial_followup_edges: IndexMap<EdgeIndex, IndexSet<EdgeIndex>>,
    source_data: SourceFederatedQueryGraphs,
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

#[derive(Debug)]
pub(crate) struct ConditionNormalizedSelectionSet(NormalizedSelectionSet);
