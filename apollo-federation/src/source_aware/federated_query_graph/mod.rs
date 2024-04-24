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
    SourceFederatedEnumQueryGraphNode, SourceFederatedLookupQueryGraphEdge,
    SourceFederatedQueryGraphs, SourceFederatedScalarQueryGraphNode,
    SourceFederatedTypeConditionQueryGraphEdge, SourceId,
};
use apollo_compiler::schema::NamedType;
use indexmap::{IndexMap, IndexSet};
use petgraph::graph::{DiGraph, EdgeIndex, NodeIndex};

pub(crate) mod builder;

#[derive(Debug)]
pub struct FederatedQueryGraph {
    graph: DiGraph<FederatedQueryGraphNode, FederatedQueryGraphEdge>,
    supergraph_types_to_nodes: IndexMap<NamedType, IndexSet<NodeIndex>>,
    supergraph_root_kinds_to_nodes: IndexMap<SchemaRootDefinitionKind, NodeIndex>,
    self_conditions: Vec<NormalizedSelectionSet>,
    non_trivial_followup_edges: IndexMap<EdgeIndex, IndexSet<EdgeIndex>>,
    source_data: SourceFederatedQueryGraphs,
}

#[derive(Debug)]
pub(crate) enum FederatedQueryGraphNode {
    Abstract {
        supergraph_type: AbstractTypeDefinitionPosition,
        fields: IndexMap<AbstractFieldDefinitionPosition, IndexSet<EdgeIndex>>,
        type_conditions: IndexMap<CompositeTypeDefinitionPosition, IndexSet<EdgeIndex>>,
        lookups: IndexMap<NodeIndex, IndexSet<EdgeIndex>>,
        source_id: SourceId,
        source_data: SourceFederatedAbstractQueryGraphNode,
    },
    Concrete {
        supergraph_type: ObjectTypeDefinitionPosition,
        fields: IndexMap<ObjectFieldDefinitionPosition, IndexSet<EdgeIndex>>,
        lookups: IndexMap<NodeIndex, IndexSet<EdgeIndex>>,
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

    pub(crate) fn source_id(&self) -> &SourceId {
        match self {
            FederatedQueryGraphNode::Abstract { source_id, .. } => source_id,
            FederatedQueryGraphNode::Concrete { source_id, .. } => source_id,
            FederatedQueryGraphNode::Enum { source_id, .. } => source_id,
            FederatedQueryGraphNode::Scalar { source_id, .. } => source_id,
        }
    }
}

#[derive(Debug)]
pub(crate) enum FederatedQueryGraphEdge {
    AbstractField {
        supergraph_field: AbstractFieldDefinitionPosition,
        self_conditions: Option<ConditionNormalizedSelectionSet>,
        matches_concrete_options: bool,
        source_id: SourceId,
        source_data: Option<SourceFederatedAbstractFieldQueryGraphEdge>,
    },
    ConcreteField {
        supergraph_field: ObjectFieldDefinitionPosition,
        self_conditions: Option<ConditionNormalizedSelectionSet>,
        source_id: SourceId,
        source_data: Option<SourceFederatedConcreteFieldQueryGraphEdge>,
    },
    TypeCondition {
        supergraph_type: CompositeTypeDefinitionPosition,
        source_id: SourceId,
        source_data: Option<SourceFederatedTypeConditionQueryGraphEdge>,
    },
    Lookup {
        supergraph_type: ObjectTypeDefinitionPosition,
        self_conditions: Option<ConditionNormalizedSelectionSet>,
        source_id: SourceId,
        source_data: Option<SourceFederatedLookupQueryGraphEdge>,
    },
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) struct SelfConditionIndex(usize);

#[derive(Debug)]
pub(crate) struct ConditionNormalizedSelectionSet(NormalizedSelectionSet);
