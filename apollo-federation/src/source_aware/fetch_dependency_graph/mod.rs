use std::sync::Arc;

use apollo_compiler::executable::Name;
use indexmap::IndexMap;
use indexmap::IndexSet;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;
use petgraph::stable_graph::StableDiGraph;

use crate::operation::SelectionSet;
use crate::source_aware::federated_query_graph::graph_path::ConditionResolutionId;
use crate::source_aware::federated_query_graph::FederatedQueryGraph;
use crate::source_aware::query_plan::FetchDataPathElement;
use crate::sources::source;
use crate::sources::source::SourceId;

#[derive(Debug)]
pub(crate) struct FetchDependencyGraph {
    query_graph: Arc<FederatedQueryGraph>,
    graph: FetchDependencyGraphPetgraph,
    root_nodes_by_source: IndexMap<SourceId, IndexSet<NodeIndex>>,
    is_reduced: bool,
    condition_resolutions_to_selection_sets: IndexMap<ConditionResolutionId, SelectionSet>,
    condition_resolutions_to_dependent_nodes: IndexMap<ConditionResolutionId, IndexSet<NodeIndex>>,
    condition_resolutions_to_containing_nodes: IndexMap<ConditionResolutionId, IndexSet<NodeIndex>>,
    source_data: source::fetch_dependency_graph::FetchDependencyGraphs,
}

type FetchDependencyGraphPetgraph = StableDiGraph<Arc<Node>, Arc<Edge>>;

#[derive(Debug)]
pub(crate) struct Node {
    merge_at: Arc<[FetchDataPathElement]>,
    source_entering_edge: EdgeIndex,
    operation_variables: IndexSet<Name>,
    depends_on_condition_resolutions: IndexSet<ConditionResolutionId>,
    contains_condition_resolutions: IndexSet<ConditionResolutionId>,
    source_id: SourceId,
    source_data: source::fetch_dependency_graph::Node,
}

#[derive(Debug)]
pub(crate) struct Edge;
