pub(crate) mod federated_query_graph;
pub(crate) mod fetch_dependency_graph;
mod models;
mod selection_parser;
pub(crate) mod spec;
mod url_path_template;

use std::fmt::Display;
use std::sync::Arc;

use apollo_compiler::executable::Name;
use apollo_compiler::executable::Value;
use apollo_compiler::Node;
use apollo_compiler::NodeStr;
use indexmap::IndexMap;
use indexmap::IndexSet;
use petgraph::graph::EdgeIndex;
pub use selection_parser::ApplyTo;
pub use selection_parser::ApplyToError;
pub use selection_parser::Selection;
pub use selection_parser::SubSelection;
pub(crate) use spec::ConnectSpecDefinition;
pub use url_path_template::URLPathTemplate;

use crate::error::FederationError;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::source_aware::federated_query_graph::graph_path::ConditionResolutionId;
use crate::source_aware::federated_query_graph::graph_path::OperationPathElement;
use crate::source_aware::federated_query_graph::FederatedQueryGraph;
use crate::source_aware::federated_query_graph::SelfConditionIndex;
use crate::source_aware::query_plan::FetchDataPathElement;
use crate::sources::connect::selection_parser::PathSelection;
use crate::sources::connect::selection_parser::Property;
use crate::sources::SourceId;
use crate::sources::SourcePath;
use crate::sources::SourcePathApi;
use crate::ValidFederationSubgraph;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct ConnectId {
    pub label: String,
    pub subgraph_name: NodeStr,
    pub directive: ObjectOrInterfaceFieldDirectivePosition,
}

impl Display for ConnectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
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
pub(crate) enum ConnectFederatedSourceEnteringQueryGraphEdge {
    ConnectParent {
        subgraph_type: ObjectTypeDefinitionPosition,
    },
}

#[derive(Debug)]
pub(crate) struct ConnectFetchDependencyGraphNode {
    merge_at: Arc<[FetchDataPathElement]>,
    source_entering_edge: EdgeIndex,
    field_response_name: Name,
    field_arguments: IndexMap<Name, Value>,
    selection: Selection,
}

#[derive(Debug)]
pub(crate) struct ConnectPath {
    merge_at: Arc<[FetchDataPathElement]>,
    source_entering_edge: EdgeIndex,
    source_id: SourceId,
    field: Option<ConnectPathField>,
}

#[derive(Debug)]
pub(crate) struct ConnectPathField {
    response_name: Name,
    arguments: IndexMap<Name, Node<Value>>,
    selections: ConnectPathSelections,
}

#[derive(Debug)]
pub(crate) enum ConnectPathSelections {
    Selections {
        head_property_path: Vec<Property>,
        named_selections: Vec<(Name, Vec<Property>)>,
        tail_selection: Option<(Name, ConnectPathTailSelection)>,
    },
    CustomScalarRoot {
        selection: Selection,
    },
}

#[derive(Debug)]
pub(crate) enum ConnectPathTailSelection {
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

impl SourcePathApi for ConnectPath {
    fn source_id(&self) -> &SourceId {
        todo!()
    }

    fn add_operation_element(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _operation_element: Arc<OperationPathElement>,
        _edge: Option<EdgeIndex>,
        _self_condition_resolutions: IndexMap<SelfConditionIndex, ConditionResolutionId>,
    ) -> Result<SourcePath, FederationError> {
        todo!()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConnectFetchNode {
    pub source_id: ConnectId,
    pub field_response_name: Name,
    pub field_arguments: IndexMap<Name, Value>,
    pub selection: Selection,
}
