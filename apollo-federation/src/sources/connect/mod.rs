mod selection_parser;
mod spec;
mod url_path_template;

use crate::error::FederationError;
use crate::schema::position::{
    EnumTypeDefinitionPosition, ObjectFieldDefinitionPosition,
    ObjectOrInterfaceFieldDirectivePosition, ObjectOrInterfaceTypeDefinitionPosition,
    ObjectTypeDefinitionPosition, ScalarTypeDefinitionPosition,
};
use crate::source_aware::federated_query_graph::builder::IntraSourceQueryGraphBuilderApi;
use crate::sources::connect::selection_parser::{PathSelection, Property, SubSelection};
use crate::sources::{FederatedLookupTailData, SourceFederatedQueryGraphBuilderApi};
use crate::ValidFederationSubgraph;
use apollo_compiler::executable::{Name, Value};
use apollo_compiler::NodeStr;
use indexmap::{IndexMap, IndexSet};
pub use selection_parser::ApplyTo;
pub use selection_parser::ApplyToError;
pub use selection_parser::Selection;
pub use url_path_template::URLPathTemplate;

pub(crate) use spec::ConnectSpecDefinition;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct ConnectId {
    subgraph_name: NodeStr,
    directive: ObjectOrInterfaceFieldDirectivePosition,
}

#[derive(Debug)]
pub(crate) struct ConnectFederatedQueryGraph {
    subgraphs_by_name: IndexMap<NodeStr, ValidFederationSubgraph>,
    // source_directives_by_name: IndexMap<NodeStr, SourceDirectiveArguments>,
    // connect_directives_by_source: IndexMap<ConnectId, ConnectDirectiveArguments>,
}

#[derive(Debug)]
pub(crate) struct ConnectFederatedAbstractQueryGraphNode;

#[derive(Debug)]
pub(crate) enum ConnectFederatedConcreteQueryGraphNode {
    ConnectParent {
        subgraph_type: ObjectTypeDefinitionPosition,
    },
    SelectionRoot {
        subgraph_type: ObjectTypeDefinitionPosition,
        property_path: Vec<Property>,
    },
    SelectionChild {
        subgraph_type: ObjectTypeDefinitionPosition,
    },
}

#[derive(Debug)]
pub(crate) enum ConnectFederatedEnumQueryGraphNode {
    SelectionRoot {
        subgraph_type: EnumTypeDefinitionPosition,
        property_path: Vec<Property>,
    },
    SelectionChild {
        subgraph_type: EnumTypeDefinitionPosition,
    },
}

#[derive(Debug)]
pub(crate) enum ConnectFederatedScalarQueryGraphNode {
    SelectionRoot {
        subgraph_type: ScalarTypeDefinitionPosition,
        property_path: Vec<Property>,
    },
    CustomScalarSelectionRoot {
        subgraph_type: ScalarTypeDefinitionPosition,
        selection: Selection,
    },
    SelectionChild {
        subgraph_type: ScalarTypeDefinitionPosition,
    },
}

#[derive(Debug)]
pub(crate) struct ConnectFederatedAbstractFieldQueryGraphEdge;

#[derive(Debug)]

pub(crate) enum ConnectFederatedConcreteFieldQueryGraphEdge {
    Connect {
        subgraph_field: ObjectFieldDefinitionPosition,
    },
    Selection {
        subgraph_field: ObjectFieldDefinitionPosition,
        property_path: Vec<Property>,
    },
    CustomScalarPathSelection {
        subgraph_field: ObjectFieldDefinitionPosition,
        path_selection: PathSelection,
    },
    CustomScalarStarSelection {
        subgraph_field: ObjectFieldDefinitionPosition,
        star_subselection: Option<SubSelection>,
        excluded_properties: IndexSet<Property>,
    },
}

#[derive(Debug)]
pub(crate) struct ConnectFederatedTypeConditionQueryGraphEdge;

#[derive(Debug)]
pub(crate) enum ConnectFederatedLookupQueryGraphEdge {
    ConnectParent {
        subgraph_type: ObjectOrInterfaceTypeDefinitionPosition,
    },
}

pub(crate) struct ConnectFederatedQueryGraphBuilder;

impl SourceFederatedQueryGraphBuilderApi for ConnectFederatedQueryGraphBuilder {
    fn process_subgraph_schema(
        &self,
        _subgraph: ValidFederationSubgraph,
        _builder: &mut impl IntraSourceQueryGraphBuilderApi,
    ) -> Result<Vec<FederatedLookupTailData>, FederationError> {
        todo!()
    }
}

#[derive(Debug)]
pub struct ConnectFetchNode {
    source_id: ConnectId,
    arguments: IndexMap<Name, Value>,
    selection: Selection,
}
