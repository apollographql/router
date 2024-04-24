use crate::error::FederationError;
use crate::schema::position::{
    AbstractFieldDefinitionPosition, AbstractTypeDefinitionPosition,
    CompositeTypeDefinitionPosition, EnumTypeDefinitionPosition, ObjectFieldDefinitionPosition,
    ObjectOrInterfaceFieldDirectivePosition, ObjectTypeDefinitionPosition,
    ScalarTypeDefinitionPosition, SchemaRootDefinitionKind,
};
use crate::source_aware::federated_query_graph::builder::IntraSourceQueryGraphBuilderApi;
use crate::source_aware::federated_query_graph::SelfConditionIndex;
use crate::sources::{FederatedLookupTailData, SourceFederatedQueryGraphBuilderApi};
use crate::ValidFederationSubgraph;
use apollo_compiler::executable::OperationType;
use apollo_compiler::validation::Valid;
use apollo_compiler::{ExecutableDocument, NodeStr};
use indexmap::IndexMap;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct GraphqlId {
    subgraph_name: NodeStr,
}

#[derive(Debug)]
pub(crate) struct GraphqlFederatedQueryGraph {
    subgraphs_by_source: IndexMap<GraphqlId, ValidFederationSubgraph>,
}

#[derive(Debug)]
pub(crate) struct GraphqlFederatedAbstractQueryGraphNode {
    subgraph_type: AbstractTypeDefinitionPosition,
    provides_directive: Option<ObjectOrInterfaceFieldDirectivePosition>,
}

#[derive(Debug)]
pub(crate) struct GraphqlFederatedConcreteQueryGraphNode {
    subgraph_type: ObjectTypeDefinitionPosition,
    provides_directive: Option<ObjectOrInterfaceFieldDirectivePosition>,
}

#[derive(Debug)]
pub(crate) struct GraphqlFederatedEnumQueryGraphNode {
    subgraph_type: EnumTypeDefinitionPosition,
}

#[derive(Debug)]
pub(crate) struct GraphqlFederatedScalarQueryGraphNode {
    subgraph_type: ScalarTypeDefinitionPosition,
}

#[derive(Debug)]
pub(crate) struct GraphqlFederatedAbstractFieldQueryGraphEdge {
    subgraph_field: AbstractFieldDefinitionPosition,
}

#[derive(Debug)]
pub(crate) struct GraphqlFederatedConcreteFieldQueryGraphEdge {
    subgraph_field: ObjectFieldDefinitionPosition,
    requires_condition: Option<SelfConditionIndex>,
}

#[derive(Debug)]
pub(crate) struct GraphqlFederatedTypeConditionQueryGraphEdge {
    subgraph_type: CompositeTypeDefinitionPosition,
}

#[derive(Debug)]
pub(crate) enum GraphqlFederatedLookupQueryGraphEdge {
    OperationRoot {
        subgraph_type: ObjectTypeDefinitionPosition,
        root_kind: SchemaRootDefinitionKind,
    },
    EntitiesField {
        subgraph_type: ObjectTypeDefinitionPosition,
        key_condition: Option<SelfConditionIndex>,
    },
}

pub(crate) struct GraphqlFederatedQueryGraphBuilder;

impl SourceFederatedQueryGraphBuilderApi for GraphqlFederatedQueryGraphBuilder {
    fn process_subgraph_schema(
        &self,
        _subgraph: ValidFederationSubgraph,
        _builder: &mut impl IntraSourceQueryGraphBuilderApi,
    ) -> Result<Vec<FederatedLookupTailData>, FederationError> {
        todo!()
    }
}

#[derive(Debug)]
pub struct GraphqlFetchNode {
    source_id: GraphqlId,
    operation_document: Valid<ExecutableDocument>,
    operation_name: Option<NodeStr>,
    operation_kind: OperationType,
}
