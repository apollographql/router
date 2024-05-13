use apollo_compiler::NodeStr;
use indexmap::IndexMap;
use indexmap::IndexSet;

use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::ObjectFieldDefinitionPosition;
use crate::sources::connect::json_selection::JSONSelection;
use crate::sources::connect::json_selection::Key;
use crate::sources::connect::json_selection::PathSelection;
use crate::sources::connect::json_selection::SubSelection;
use crate::sources::source;
use crate::sources::source::federated_query_graph::FederatedQueryGraphApi;
use crate::sources::source::SourceId;
use crate::ValidFederationSubgraph;

pub(crate) mod builder;

#[derive(Debug)]
pub(crate) struct FederatedQueryGraph {
    subgraphs_by_name: IndexMap<NodeStr, ValidFederationSubgraph>,
    // source_directives_by_name: IndexMap<NodeStr, SourceDirectiveArguments>,
    // connect_directives_by_source: IndexMap<ConnectId, ConnectDirectiveArguments>,
}

impl FederatedQueryGraphApi for FederatedQueryGraph {
    fn execution_metadata(
        &self,
    ) -> IndexMap<SourceId, source::query_plan::query_planner::ExecutionMetadata> {
        todo!()
    }
}

#[derive(Debug)]
pub(crate) struct AbstractNode;

#[derive(Debug)]
pub(crate) enum ConcreteNode {
    ConnectParent {
        subgraph_type: ObjectTypeDefinitionPosition,
    },
    SelectionRoot {
        subgraph_type: ObjectTypeDefinitionPosition,
        property_path: Vec<Key>,
    },
    SelectionChild {
        subgraph_type: ObjectTypeDefinitionPosition,
    },
}

#[derive(Debug)]
pub(crate) enum EnumNode {
    SelectionRoot {
        subgraph_type: EnumTypeDefinitionPosition,
        property_path: Vec<Key>,
    },
    SelectionChild {
        subgraph_type: EnumTypeDefinitionPosition,
    },
}

#[derive(Debug)]
pub(crate) enum ScalarNode {
    SelectionRoot {
        subgraph_type: ScalarTypeDefinitionPosition,
        property_path: Vec<Key>,
    },
    CustomScalarSelectionRoot {
        subgraph_type: ScalarTypeDefinitionPosition,
        selection: JSONSelection,
    },
    SelectionChild {
        subgraph_type: ScalarTypeDefinitionPosition,
    },
}

#[derive(Debug)]
pub(crate) struct AbstractFieldEdge;

#[derive(Debug)]

pub(crate) enum ConcreteFieldEdge {
    Connect {
        subgraph_field: ObjectFieldDefinitionPosition,
    },
    Selection {
        subgraph_field: ObjectFieldDefinitionPosition,
        property_path: Vec<Key>,
    },
    CustomScalarPathSelection {
        subgraph_field: ObjectFieldDefinitionPosition,
        path_selection: PathSelection,
    },
    CustomScalarStarSelection {
        subgraph_field: ObjectFieldDefinitionPosition,
        star_subselection: Option<SubSelection>,
        excluded_properties: IndexSet<Key>,
    },
}

#[derive(Debug)]
pub(crate) struct TypeConditionEdge;

#[derive(Debug)]
pub(crate) enum SourceEnteringEdge {
    ConnectParent {
        subgraph_type: ObjectTypeDefinitionPosition,
    },
}
