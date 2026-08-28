//! Field-by-field query planner using BULB (Beam search Using Limited
//! discrepancy Backtracking).
//!
//! The planner walks the operation one selection at a time, routing each
//! field or inline fragment to a subgraph via the federated query graph.
//! It replaces the traversal-based planner's all-at-once approach with an
//! incremental one: each selection is a decision point, and BULB explores
//! alternatives only where the greedy choice is demonstrably suboptimal.
//!
//! # Architecture
//!
//! The system is layered bottom-up:
//!
//! - [`bulb_search`]: Generic beam search engine parameterized by a
//!   `BulbSearchSpace` trait. Knows nothing about federation.
//!
//! - [`shared_path`]: Immutable, structurally-shared path segments used
//!   by the fetch graph to track where selections sit in the response.
//!
//! - [`fetch_graph`]: Mutable graph of fetch groups (subgraph calls)
//!   with an undo log for checkpoint/rollback during search. Each node
//!   is a fetch group; edges encode data dependencies.
//!
//! - [`field_routing`]: The `BulbSearchSpace` implementation. Routes
//!   selections through the query graph, enumerating subgraph edges and
//!   key hops as options, committing choices into the fetch graph, and
//!   managing the pending-selection stack.
//!
//! - This module: entry point ([`build_bulb_plan`]) that seeds the
//!   initial state from the operation root and materializes the
//!   finished fetch graph into a `QueryPlan`.
//!
//! # Flow
//!
//! `build_bulb_plan` constructs a `FieldRoutingSearchSpace` and an
//! initial `PlanState` (pending stack seeded from the operation root),
//! then hands both to `bulb_search`. The search alternates between
//! fast-forwarding (greedily committing single-option selections) and
//! branching at multi-option decision points. When complete, the
//! `FetchGraph` in the winning state is converted to a `QueryPlan` via
//! the plan builder.

pub mod bulb_search;
pub(crate) mod fetch_graph;
pub(crate) mod field_routing;
pub mod shared_path;

use bulb_search::BulbConfig;
use bulb_search::bulb_search;
use fetch_graph::FetchGraph;
use field_routing::FieldRoutingSearchSpace;
use field_routing::state::PendingSelection;
use field_routing::state::PlanState;
use petgraph::graph::NodeIndex;
use tracing::debug;

use crate::error::FederationError;
use crate::operation::SelectionSet;
use crate::query_graph::QueryGraphNodeType;
use crate::query_plan::PlanNode;
use crate::query_plan::QueryPlanCost;
use crate::query_plan::query_planner::SubgraphOperationCompression;
use crate::query_plan::query_planning_traversal::QueryPlanningParameters;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;

/// The BULB planner's result: the materialized plan (None when the
/// operation resolves to nothing) and its structural cost.
pub(crate) struct BulbPlan {
    pub(crate) plan: Option<PlanNode>,
    pub(crate) cost: QueryPlanCost,
}

/// Entry point for the field-routing BULB planner: drives query planning
/// field-by-field via BULB on the `FieldRoutingSearchSpace`.
#[tracing::instrument(level = "debug", skip_all, name = "build_bulb_plan")]
pub(crate) fn build_bulb_plan(
    parameters: &QueryPlanningParameters,
    selection_set: &SelectionSet,
    root_kind: SchemaRootDefinitionKind,
    _has_defers: bool,
) -> Result<BulbPlan, FederationError> {
    debug!(
        selections = selection_set.selections.len(),
        root_kind = ?root_kind,
        "entering build_bulb_plan",
    );

    let query_graph = &parameters.federated_query_graph;
    let supergraph_schema = &parameters.supergraph_schema;

    // Normalization strips __typename selections and tags a sibling instead
    // (`optimize_sibling_typenames`). BULB never branches on __typename, so
    // restore the stripped selections up front and route them like any other
    // field.
    let selection_set = selection_set.add_back_typename_in_attachments()?;
    let selection_set = &selection_set;

    let search_space = FieldRoutingSearchSpace {
        query_graph: query_graph.clone(),
        supergraph_schema: supergraph_schema.clone(),
        override_conditions: parameters.override_conditions.clone(),
        inconsistent_abstract_types: parameters
            .abstract_types_with_inconsistent_runtime_types
            .clone(),
    };

    let root_qg_node = parameters.head;
    let root_node_data = query_graph.node_weight(root_qg_node)?;
    match &root_node_data.type_ {
        QueryGraphNodeType::SchemaType(pos) => {
            let root_type: CompositeTypeDefinitionPosition = pos.clone().try_into()?;

            let mut graph = FetchGraph::new();
            let fetch_node = graph.get_or_create_root_group(&root_node_data.source, root_type);
            let pending = root_pending_selections(selection_set, root_qg_node, fetch_node);
            let initial = PlanState::with_graph(graph, pending);
            run_bulb_and_finalize(&search_space, parameters, initial, root_kind)
        }
        QueryGraphNodeType::FederatedRootType(_) => {
            build_bulb_plan_from_federated_root(&search_space, parameters, selection_set, root_kind)
        }
    }
}

/// Handle a FederatedRootType head (fans out to per-subgraph roots via
/// `SubgraphEnteringTransition` edges): one `PendingSelection` per
/// top-level field.
fn build_bulb_plan_from_federated_root(
    search_space: &FieldRoutingSearchSpace,
    parameters: &QueryPlanningParameters,
    selection_set: &SelectionSet,
    root_kind: SchemaRootDefinitionKind,
) -> Result<BulbPlan, FederationError> {
    let root_qg_node = parameters.head;

    // The fetch_node placeholder is unused: commit_choice computes the
    // actual root fetch group from the chosen subgraph.
    let pending = root_pending_selections(selection_set, root_qg_node, NodeIndex::end());
    let initial = PlanState::new(pending);
    run_bulb_and_finalize(search_space, parameters, initial, root_kind)
}

/// Run BULB search on the initial state and finalize into a `BulbPlan`.
#[tracing::instrument(level = "debug", skip_all, name = "run_bulb_and_finalize")]
fn run_bulb_and_finalize(
    search_space: &FieldRoutingSearchSpace,
    parameters: &QueryPlanningParameters,
    initial: PlanState,
    root_kind: SchemaRootDefinitionKind,
) -> Result<BulbPlan, FederationError> {
    let config = BulbConfig {
        beam_width: parameters.config.incremental_planner.beam_width,
        fuel: parameters.config.incremental_planner.fuel,
        timeout: parameters.config.incremental_planner.timeout,
    };

    debug!(
        pending = initial.pending.len(),
        beam_width = config.beam_width,
        fuel = config.fuel,
        "starting BULB search",
    );

    let (result, stats) = bulb_search(
        search_space,
        initial,
        config,
        parameters.check_for_cooperative_cancellation,
    );

    debug!(
        pending_remaining = result.pending.len(),
        dropped_fields = result.dropped_fields,
        evaluated_plans = stats.evaluated_plans,
        expansions = stats.expansions,
        effort = stats.effort,
        timed_out = stats.timed_out,
        cancelled = stats.cancelled,
        fetch_nodes = result.graph.node_count(),
        fetch_edges = result.graph.edge_count(),
        "BULB search complete",
    );

    parameters
        .statistics
        .evaluated_plan_count
        .set(stats.evaluated_plans);

    if stats.cancelled {
        return Err(crate::error::SingleFederationError::PlanningCancelled.into());
    }

    // An incomplete plan must never be returned: executing it would
    // silently omit response fields.
    if result.dropped_fields > 0 || !result.pending.is_empty() {
        if !parameters.disabled_subgraphs.is_empty() {
            return Err(
                crate::error::SingleFederationError::NoPlanFoundWithDisabledSubgraphs.into(),
            );
        }
        return Err(FederationError::internal(format!(
            "BULB planner could not produce a complete plan: \
             {} dropped selection(s), {} unplanned selection(s)",
            result.dropped_fields,
            result.pending.len(),
        )));
    }

    let mut operation_compression = if parameters.config.generate_query_fragments {
        SubgraphOperationCompression::GenerateFragments
    } else {
        SubgraphOperationCompression::Disabled
    };

    let mut build_ctx = fetch_graph::plan_builder::PlanBuildContext {
        supergraph_schema: &parameters.supergraph_schema,
        query_graph: &parameters.federated_query_graph,
        root_kind,
        variable_definitions: &parameters.operation.variables,
        operation_directives: &parameters.operation.directives,
        operation_name: &parameters.operation.name,
        operation_compression: &mut operation_compression,
        operation_counter: 0,
    };
    let (plan, cost) = result.graph.to_query_plan(&mut build_ctx)?;

    Ok(BulbPlan { plan, cost })
}

/// One pending entry per top-level selection, anchored at the operation
/// root and reversed so the first selection is popped first.
fn root_pending_selections(
    selection_set: &SelectionSet,
    root_qg_node: NodeIndex,
    fetch_node: NodeIndex,
) -> Vec<PendingSelection> {
    selection_set
        .selections
        .values()
        .rev()
        .map(|sel| PendingSelection {
            selection: sel.clone(),
            query_graph_node: root_qg_node,
            fetch_node,
            op_path: Default::default(),
            path_in_fetch: Default::default(),
            condition: None,
        })
        .collect()
}
