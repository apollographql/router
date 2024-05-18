use indexmap::IndexMap;

use crate::schema::position::AbstractFieldDefinitionPosition;
use crate::schema::position::AbstractTypeDefinitionPosition;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::schema::ObjectFieldDefinitionPosition;
use crate::schema::ObjectOrInterfaceFieldDirectivePosition;
use crate::source_aware::federated_query_graph::SelfConditionIndex;
use crate::sources::graphql::GraphqlId;
use crate::sources::source;
use crate::sources::source::federated_query_graph::FederatedQueryGraphApi;
use crate::sources::source::SourceId;
use crate::ValidFederationSubgraph;

pub(crate) mod builder;

#[derive(Debug, Default)]
pub(crate) struct FederatedQueryGraph {
    subgraphs_by_source: IndexMap<GraphqlId, ValidFederationSubgraph>,
}

impl FederatedQueryGraphApi for FederatedQueryGraph {
    fn execution_metadata(
        &self,
    ) -> IndexMap<SourceId, source::query_plan::query_planner::ExecutionMetadata> {
        todo!()
    }
}

#[derive(Debug)]
pub(crate) struct AbstractNode {
    subgraph_type: AbstractTypeDefinitionPosition,
    provides_directive: Option<ObjectOrInterfaceFieldDirectivePosition>,
}

#[derive(Debug)]
pub(crate) struct ConcreteNode {
    subgraph_type: ObjectTypeDefinitionPosition,
    provides_directive: Option<ObjectOrInterfaceFieldDirectivePosition>,
}

#[derive(Debug)]
pub(crate) struct EnumNode {
    subgraph_type: EnumTypeDefinitionPosition,
}

#[derive(Debug)]
pub(crate) struct ScalarNode {
    subgraph_type: ScalarTypeDefinitionPosition,
}

#[derive(Debug)]
pub(crate) struct AbstractFieldEdge {
    subgraph_field: AbstractFieldDefinitionPosition,
}

#[derive(Debug)]
pub(crate) struct ConcreteFieldEdge {
    subgraph_field: ObjectFieldDefinitionPosition,
    requires_condition: Option<SelfConditionIndex>,
}

#[derive(Debug)]
pub(crate) struct TypeConditionEdge {
    subgraph_type: CompositeTypeDefinitionPosition,
}

#[derive(Debug)]
pub(crate) enum SourceEnteringEdge {
    OperationRoot {
        subgraph_type: ObjectTypeDefinitionPosition,
        root_kind: SchemaRootDefinitionKind,
    },
    EntitiesField {
        subgraph_type: ObjectTypeDefinitionPosition,
        key_condition: Option<SelfConditionIndex>,
    },
}
