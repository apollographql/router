use crate::query_plan::operation::NormalizedSelectionSet;
use crate::source_aware::federated_query_graph::graph_path::ConditionResolutionId;
use crate::source_aware::federated_query_graph::FederatedQueryGraph;
use crate::source_aware::query_plan::FetchDataPathElement;
use crate::sources::{SourceFetchDependencyGraphNode, SourceFetchDependencyGraphs, SourceId};
use apollo_compiler::executable::Name;
use indexmap::{IndexMap, IndexSet};
use petgraph::graph::{EdgeIndex, NodeIndex};
use petgraph::stable_graph::StableDiGraph;
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct FetchDependencyGraph {
    query_graph: Arc<FederatedQueryGraph>,
    graph: FetchDependencyGraphPetgraph,
    root_nodes_by_source: IndexMap<SourceId, IndexSet<NodeIndex>>,
    is_reduced: bool,
    condition_resolutions_to_selection_sets:
        IndexMap<ConditionResolutionId, NormalizedSelectionSet>,
    condition_resolutions_to_dependent_nodes: IndexMap<ConditionResolutionId, IndexSet<NodeIndex>>,
    condition_resolutions_to_containing_nodes: IndexMap<ConditionResolutionId, IndexSet<NodeIndex>>,
    source_data: SourceFetchDependencyGraphs,
}

type FetchDependencyGraphPetgraph =
    StableDiGraph<Arc<FetchDependencyGraphNode>, Arc<FetchDependencyGraphEdge>>;

#[derive(Debug)]
pub(crate) struct FetchDependencyGraphNode {
    merge_at: Vec<FetchDataPathElement>,
    source_enter_edge: EdgeIndex,
    operation_variables: IndexSet<Name>,
    depends_on_condition_resolutions: IndexSet<ConditionResolutionId>,
    contains_condition_resolutions: IndexSet<ConditionResolutionId>,
    source_id: SourceId,
    source_data: SourceFetchDependencyGraphNode,
}

#[derive(Debug)]
pub(crate) struct FetchDependencyGraphEdge;
