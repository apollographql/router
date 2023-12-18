use crate::query_graph::graph_path::{ClosedBranch, OpenBranch};
use crate::query_graph::path_tree::OpPathTree;
use crate::query_graph::QueryGraph;
use crate::query_plan::fetch_dependency_graph::FetchDependencyGraph;
use crate::query_plan::fetch_dependency_graph_processor::{
    FetchDependencyGraphToCostProcessor, FetchDependencyGraphToQueryPlanProcessor,
};
use crate::query_plan::operation::{NormalizedOperation, NormalizedSelection};
use crate::query_plan::query_planner::QueryPlannerConfig;
use crate::query_plan::QueryPlanCost;
use crate::schema::position::{AbstractTypeDefinitionPosition, SchemaRootDefinitionKind};
use crate::schema::ValidFederationSchema;
use indexmap::IndexSet;
use petgraph::graph::NodeIndex;
use std::sync::Arc;

// PORT_NOTE: Named `PlanningParameters` in the JS codebase, but there was no particular reason to
// leave out to the `Query` prefix, so it's been added for consistency. Similar to `GraphPath`, we
// don't have a distinguished type for when the head is a root vertex, so we instead check this at
// runtime (introducing the new field `head_must_be_root`).
pub(crate) struct QueryPlanningParameters {
    /// The supergraph schema that generated the federated query graph.
    supergraph_schema: ValidFederationSchema,
    /// The federated query graph used for query planning.
    federated_query_graph: Arc<QueryGraph>,
    /// The operation to be query planned.
    operation: Arc<NormalizedOperation>,
    /// A processor for converting fetch dependency graphs to query plans.
    processor: FetchDependencyGraphToQueryPlanProcessor,
    /// The query graph node at which query planning begins.
    head: NodeIndex,
    /// Whether the head must be a root node for query planning.
    head_must_be_root: bool,
    /// A set of the names of interface or union types that have inconsistent "runtime types" across
    /// subgraphs.
    // PORT_NOTE: Named `inconsistentAbstractTypesRuntimes` in the JS codebase, which was slightly
    // confusing.
    abstract_types_with_inconsistent_runtime_types: Arc<IndexSet<AbstractTypeDefinitionPosition>>,
    /// The configuration for the query planner.
    config: Arc<QueryPlannerConfig>,
    // TODO: When `PlanningStatistics` is ported, add a field for it.
}

// PORT_NOTE: The JS codebase also had a field `conditionResolver`, but this was only ever used once
// during construction, so we don't store it in the struct itself.
pub(crate) struct QueryPlanningTraversal {
    /// The parameters given to query planning.
    parameters: QueryPlanningParameters,
    /// The root kind of the operation.
    root_kind: SchemaRootDefinitionKind,
    /// True if query planner `@defer` support is enabled and the operation contains some `@defer`
    /// application.
    has_defers: bool,
    /// The initial fetch ID generation (used when handling `@defer`).
    starting_id_generation: u64,
    /// A processor for converting fetch dependency graphs to cost.
    cost_processor: FetchDependencyGraphToCostProcessor,
    /// True if this query planning is at top-level (note that query planning can recursively start
    /// further query planning).
    is_top_level: bool,
    /// The stack of open branches left to plan, along with state indicating the next selection to
    /// plan for them.
    // PORT_NOTE: The `stack` in the JS codebase only contained one selection per stack entry, but
    // to avoid having to clone the `OpenBranch` structures (which loses the benefits of indirect
    // path caching), we create a multi-level-stack here, where the top-level stack is over open
    // branches and the sub-stack is over selections.
    open_branches: Vec<OpenBranchAndSelections>,
    /// The closed branches that have been planned.
    closed_branches: Vec<ClosedBranch>,
    /// The best plan found as a result of query planning.
    best_plan: Option<BestQueryPlanInfo>,
}

struct OpenBranchAndSelections {
    /// The options for this open branch.
    open_branch: OpenBranch,
    /// A stack of the remaining selections to plan from the node this open branch ends on.
    selections: Vec<NormalizedSelection>,
}

struct BestQueryPlanInfo {
    /// The fetch dependency graph for this query plan.
    fetch_dependency_graph: FetchDependencyGraph,
    /// The path tree for the closed branch options chosen for this query plan.
    path_tree: OpPathTree,
    /// The cost of this query plan.
    cost: QueryPlanCost,
}
