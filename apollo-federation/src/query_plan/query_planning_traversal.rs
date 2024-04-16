use crate::error::FederationError;
use crate::query_graph::condition_resolver::CachingConditionResolver;
use crate::query_graph::graph_path::{
    ClosedBranch, ClosedPath, OpPathElement, OpenBranch, SimultaneousPaths,
    SimultaneousPathsWithLazyIndirectPaths,
};
use crate::query_graph::path_tree::OpPathTree;
use crate::query_graph::{QueryGraph, QueryGraphNodeType};
use crate::query_plan::fetch_dependency_graph::{compute_nodes_for_tree, FetchDependencyGraph};
use crate::query_plan::fetch_dependency_graph_processor::{
    FetchDependencyGraphToCostProcessor, FetchDependencyGraphToQueryPlanProcessor,
};
use crate::query_plan::operation::{
    NormalizedOperation, NormalizedSelection, NormalizedSelectionSet,
};
use crate::query_plan::query_planner::QueryPlannerConfig;
use crate::query_plan::query_planner::QueryPlanningStatistics;
use crate::query_plan::QueryPlanCost;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::schema::position::{AbstractTypeDefinitionPosition, OutputTypeDefinitionPosition};
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
    pub(crate) supergraph_schema: ValidFederationSchema,
    /// The federated query graph used for query planning.
    pub(crate) federated_query_graph: Arc<QueryGraph>,
    /// The operation to be query planned.
    pub(crate) operation: Arc<NormalizedOperation>,
    /// A processor for converting fetch dependency graphs to query plans.
    pub(crate) processor: FetchDependencyGraphToQueryPlanProcessor,
    /// The query graph node at which query planning begins.
    pub(crate) head: NodeIndex,
    /// Whether the head must be a root node for query planning.
    pub(crate) head_must_be_root: bool,
    /// A set of the names of interface or union types that have inconsistent "runtime types" across
    /// subgraphs.
    // PORT_NOTE: Named `inconsistentAbstractTypesRuntimes` in the JS codebase, which was slightly
    // confusing.
    pub(crate) abstract_types_with_inconsistent_runtime_types:
        Arc<IndexSet<AbstractTypeDefinitionPosition>>,
    /// The configuration for the query planner.
    pub(crate) config: QueryPlannerConfig,
    pub(crate) statistics: QueryPlanningStatistics,
}

pub(crate) struct QueryPlanningTraversal<'a> {
    /// The parameters given to query planning.
    parameters: &'a QueryPlanningParameters,
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
    /// A query plan resolver for edge conditions that caches the outcome per edge.
    condition_resolver: CachingConditionResolver,
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

pub(crate) struct BestQueryPlanInfo {
    /// The fetch dependency graph for this query plan.
    pub fetch_dependency_graph: FetchDependencyGraph,
    /// The path tree for the closed branch options chosen for this query plan.
    pub path_tree: OpPathTree,
    /// The cost of this query plan.
    pub cost: QueryPlanCost,
}

impl BestQueryPlanInfo {
    // PORT_NOTE: The equivalent of `createEmptyPlan` in the JS codebase.
    pub fn empty(parameters: &QueryPlanningParameters) -> Self {
        Self {
            fetch_dependency_graph: FetchDependencyGraph::new(
                parameters.supergraph_schema.clone(),
                parameters.federated_query_graph.clone(),
                None,
                0,
            ),
            path_tree: OpPathTree::new(parameters.federated_query_graph.clone(), parameters.head),
            cost: Default::default(),
        }
    }
}

impl<'a> QueryPlanningTraversal<'a> {
    pub fn new(
        // TODO(@goto-bus-stop): This probably needs a mutable reference for some of the
        // yet-unimplemented methods, and storing a mutable ref in `Self` here smells bad.
        // The ownership of `QueryPlanningParameters` is awkward and should probably be
        // refactored.
        parameters: &'a QueryPlanningParameters,
        _selection_set: NormalizedSelectionSet,
        has_defers: bool,
        root_kind: SchemaRootDefinitionKind,
        cost_processor: FetchDependencyGraphToCostProcessor,
    ) -> Self {
        // FIXME(@goto-bus-stop): Is this correct?
        let is_top_level = parameters.head_must_be_root;
        Self {
            parameters,
            root_kind,
            has_defers,
            starting_id_generation: 0,
            cost_processor,
            is_top_level,
            // TODO: Use `self.resolve_condition_plan()` once it exists. See FED-46.
            condition_resolver: CachingConditionResolver,
            // TODO: In JS this calls `createInitialOptions()`. Do we still need that? See FED-147.
            open_branches: Default::default(),
            closed_branches: Default::default(),
            best_plan: None,
        }
    }

    // PORT_NOTE: In JS, the traversal is still usable after finding the best plan. Here we consume
    // the struct so we do not need to return a reference, which is very unergonomic.
    pub fn find_best_plan(mut self) -> Result<Option<BestQueryPlanInfo>, FederationError> {
        self.find_best_plan_inner()?;
        Ok(self.best_plan)
    }

    fn find_best_plan_inner(&mut self) -> Result<Option<&BestQueryPlanInfo>, FederationError> {
        while let Some(mut current_branch) = self.open_branches.pop() {
            let Some(current_selection) = current_branch.selections.pop() else {
                return Err(FederationError::internal(
                    "Sub-stack unexpectedly empty during query plan traversal",
                ));
            };
            let (terminate_planning, new_branch) =
                self.handle_open_branch(&current_selection, &mut current_branch.open_branch.0)?;
            if terminate_planning {
                // We clear both open branches and closed ones as a means to terminate the plan
                // computation with no plan.
                self.open_branches = vec![];
                self.closed_branches = vec![];
                break;
            }
            if !current_branch.selections.is_empty() {
                self.open_branches.push(current_branch);
            }
            if let Some(new_branch) = new_branch {
                self.open_branches.push(new_branch);
            }
        }
        self.compute_best_plan_from_closed_branches()?;
        return Ok(self.best_plan.as_ref());
    }

    /// Returns whether to terminate planning immediately, and any new open branches to push onto
    /// the stack.
    fn handle_open_branch(
        &mut self,
        selection: &NormalizedSelection,
        options: &mut Vec<SimultaneousPathsWithLazyIndirectPaths>,
    ) -> Result<(bool, Option<OpenBranchAndSelections>), FederationError> {
        let operation_element = selection.element()?;
        let mut new_options = vec![];
        let mut no_followups: bool = false;
        for option in options.iter_mut() {
            let followups_for_option = option.advance_with_operation_element(
                self.parameters.supergraph_schema.clone(),
                &operation_element,
                &mut self.condition_resolver,
            )?;
            let Some(followups_for_option) = followups_for_option else {
                // There is no valid way to advance the current operation element from this option
                // so this option is a dead branch that cannot produce a valid query plan. So we
                // simply ignore it and rely on other options.
                continue;
            };
            if followups_for_option.is_empty() {
                // See the comment above where we check `no_followups` for more information.
                no_followups = true;
                break;
            }
            new_options.extend(followups_for_option);
            if let Some(options_limit) = self.parameters.config.debug.paths_limit {
                if new_options.len() > options_limit as usize {
                    // TODO: Create a new error code for this error kind.
                    return Err(FederationError::internal(format!(
                        "Too many options generated for {}, reached the limit of {}.",
                        selection, options_limit,
                    )));
                }
            }
        }

        if no_followups {
            // This operation element is valid from this option, but is guarantee to yield no result
            // (e.g. it's a type condition with no intersection with a prior type condition). Given
            // that all options return the same results (assuming the user does properly resolve all
            // versions of a given field the same way from all subgraphs), we know that the
            // operation element should return no result from all options (even if we can't provide
            // it technically).
            //
            // More concretely, this usually means the current operation element is a type condition
            // that has no intersection with the possible current runtime types at this point, and
            // this means whatever fields the type condition sub-selection selects, they will never
            // be part of the results. That said, we cannot completely ignore the
            // type-condition/fragment or we'd end up with the wrong results. Consider this example
            // where a sub-part of the query is:
            //   {
            //     foo {
            //       ... on Bar {
            //         field
            //       }
            //     }
            //   }
            // and suppose that `... on Bar` can never match a concrete runtime type at this point.
            // Because that's the only sub-selection of `foo`, if we completely ignore it, we'll end
            // up not querying this at all. Which means that, during execution, we'd either return
            // (for that sub-part of the query) `{ foo: null }` if `foo` happens to be nullable, or
            // just `null` for the whole sub-part otherwise. But what we *should* return (assuming
            // foo doesn't actually return `null`) is `{ foo: {} }`. Meaning, we have queried `foo`
            // and it returned something, but it's simply not a `Bar` and so nothing was included.
            //
            // Long story short, to avoid that situation, we replace the whole `... on Bar` section
            // that can never match the runtime type by simply getting the `__typename` of `foo`.
            // This ensure we do query `foo` but don't end up including conditions that may not even
            // make sense to the subgraph we're querying. Do note that we'll only need that
            // `__typename` if there is no other selections inside `foo`, and so we might include it
            // unnecessarily in practice: it's a very minor inefficiency though.
            if matches!(operation_element, OpPathElement::InlineFragment(_)) {
                let mut closed_paths = vec![];
                for option in options {
                    let mut new_simultaneous_paths = vec![];
                    for simultaneous_path in &option.paths.0 {
                        new_simultaneous_paths.push(Arc::new(
                            simultaneous_path.terminate_with_non_requested_typename_field()?,
                        ));
                    }
                    closed_paths.push(Arc::new(ClosedPath {
                        paths: SimultaneousPaths(new_simultaneous_paths),
                        selection_set: None,
                    }));
                }
                self.record_closed_branch(ClosedBranch(closed_paths))?;
            }
            return Ok((false, None));
        }

        if new_options.is_empty() {
            // If we have no options, it means there is no way to build a plan for that branch, and
            // that means the whole query planning process will generate no plan. This should never
            // happen for a top-level query planning (unless the supergraph has *not* been
            // validated), but can happen when computing sub-plans for a key condition.
            return if self.is_top_level {
                Err(FederationError::internal(format!(
                    "Was not able to find any options for {}: This shouldn't have happened.",
                    selection,
                )))
            } else {
                // Indicate to the caller that query planning should terminate with no plan.
                Ok((true, None))
            };
        }

        if let Some(selection_set) = selection.selection_set()? {
            let mut all_tail_nodes = IndexSet::new();
            for option in &new_options {
                for path in &option.paths.0 {
                    all_tail_nodes.insert(path.tail);
                }
            }
            if self.selection_set_is_fully_local_from_all_nodes(selection_set, &all_tail_nodes)?
                && !selection.has_defer()?
            {
                // We known the rest of the selection is local to whichever subgraph the current
                // options are in, and so we're going to keep that selection around and add it
                // "as-is" to the `FetchDependencyGraphNode` when needed, saving a bunch of work
                // (creating `GraphPath`, merging `PathTree`, ...). However, as we're skipping the
                // "normal path" for that sub-selection, there are a few things that are handled in
                // said "normal path" that we need to still handle.
                //
                // More precisely:
                // - We have this "attachment" trick that removes requested `__typename`
                //   temporarily, so we should add it back.
                // - We still need to add the selection of `__typename` for abstract types. It is
                //   not really necessary for the execution per-se, but if we don't do it, then we
                //   will not be able to reuse named fragments as often as we should (we add
                //   `__typename` for abstract types on the "normal path" and so we add them too to
                //   named fragments; as such, we need them here too).
                let new_selection_set = Arc::new(
                    selection_set
                        .add_back_typename_in_attachments()?
                        .add_typename_field_for_abstract_types()?,
                );
                self.record_closed_branch(ClosedBranch(
                    new_options
                        .into_iter()
                        .map(|option| {
                            Arc::new(ClosedPath {
                                paths: option.paths,
                                selection_set: Some(new_selection_set.clone()),
                            })
                        })
                        .collect(),
                ))?;
            } else {
                return Ok((
                    false,
                    Some(OpenBranchAndSelections {
                        open_branch: OpenBranch(new_options),
                        selections: selection_set.selections.values().cloned().rev().collect(),
                    }),
                ));
            }
        } else {
            self.record_closed_branch(ClosedBranch(
                new_options
                    .into_iter()
                    .map(|option| {
                        Arc::new(ClosedPath {
                            paths: option.paths,
                            selection_set: None,
                        })
                    })
                    .collect(),
            ))?;
        }

        Ok((false, None))
    }

    fn record_closed_branch(&mut self, closed_branch: ClosedBranch) -> Result<(), FederationError> {
        let maybe_trimmed = closed_branch.maybe_eliminate_strictly_more_costly_paths()?;
        self.closed_branches.push(maybe_trimmed);
        Ok(())
    }

    fn selection_set_is_fully_local_from_all_nodes(
        &self,
        _selection: &NormalizedSelectionSet,
        _nodes: &IndexSet<NodeIndex>,
    ) -> Result<bool, FederationError> {
        todo!()
    }

    fn compute_best_plan_from_closed_branches(&mut self) -> Result<(), FederationError> {
        if self.closed_branches.is_empty() {
            return Ok(());
        }
        self.prune_closed_branches();
        self.sort_options_in_closed_branches()?;
        self.reduce_options_if_needed();

        todo!() // the rest of the owl
    }

    /// Remove closed branches that are known to be overridden by others.
    ///
    /// We've computed all branches and need to compare all the possible plans to pick the best.
    /// Note however that "all the possible plans" is essentially a cartesian product of all
    /// the closed branches options, and if a lot of branches have multiple options, this can
    /// exponentially explode.
    /// So first, we check if we can preemptively prune some branches based on
    /// those branches having options that are known to be overriden by other ones.
    fn prune_closed_branches(&mut self) {
        for branch in &mut self.closed_branches {
            if branch.0.len() <= 1 {
                continue;
            }

            let mut pruned = ClosedBranch(Vec::new());
            for (i, to_check) in branch.0.iter().enumerate() {
                if !Self::option_is_overriden(i, &to_check.paths, branch) {
                    pruned.0.push(to_check.clone());
                }
            }

            *branch = pruned
        }
    }

    fn option_is_overriden(
        index: usize,
        to_check: &SimultaneousPaths,
        all_options: &ClosedBranch,
    ) -> bool {
        all_options
            .0
            .iter()
            .enumerate()
            // Don’t compare `to_check` with itself
            .filter(|&(i, _)| i != index)
            .any(|(_i, option)| {
                to_check
                    .0
                    .iter()
                    .all(|p| option.paths.0.iter().any(|o| p.is_overridden_by(o)))
            })
    }

    /// We now sort the options within each branch,
    /// putting those with the least amount of subgraph jumps first.
    /// The idea is that for each branch taken individually,
    /// the option with the least jumps is going to be the most efficient,
    /// and while it is not always the case that the best plan is built for those individual bests,
    /// they are still statistically more likely to be part of the best plan.
    /// So putting them first has 2 benefits for the rest of this method:
    ///
    /// 1. if we end up cutting some options of a branch below
    ///    (due to having too many possible plans),
    ///    we'll cut the last option first (we `pop()`),
    ///    so better cut what it the least likely to be good.
    /// 2. when we finally generate the plan,
    ///    we use the cost of previously computed plans to cut computation early when possible
    ///    (see `generate_all_plans_and_find_best`),
    ///    so there is a premium in generating good plans early (it cuts more computation),
    ///    and putting those more-likely-to-be-good options first helps this.
    fn sort_options_in_closed_branches(&mut self) -> Result<(), FederationError> {
        for branch in &mut self.closed_branches {
            let mut result = Ok(());
            branch.0.sort_by_key(|branch| {
                branch
                    .paths
                    .0
                    .iter()
                    .try_fold(0, |max_so_far, path| {
                        Ok(max_so_far.max(path.subgraph_jumps()?))
                    })
                    .unwrap_or_else(|err: FederationError| {
                        // There’s no way to abort `sort_by_key` from this callback.
                        // Store the error to be returned later and return an dummy values
                        result = Err(err);
                        0
                    })
            });
            result?
        }
        Ok(())
    }

    /// Look at how many plans we'd have to generate and if it's "too much"
    /// reduce it to something manageable by arbitrarilly throwing out options.
    /// This effectively means that when a query has too many options,
    /// we give up on always finding the "best" query plan in favor of an "ok" query plan.
    ///
    /// TODO: currently, when we need to reduce options, we do so somewhat arbitrarilly.
    /// More precisely, we reduce the branches with the most options first
    /// and then drop the last option of the branch,
    /// repeating until we have a reasonable number of plans to consider.
    /// The sorting we do about help making this slightly more likely to be a good choice,
    /// but there is likely more "smarts" we could add to this.
    fn reduce_options_if_needed(&mut self) {
        // We sort branches by those that have the most options first.
        self.closed_branches
            .sort_by(|b1, b2| b1.0.len().cmp(&b2.0.len()).reverse());
        let mut plan_count = self
            .closed_branches
            .iter()
            .try_fold(1, |product, branch| {
                if branch.0.is_empty() {
                    // This would correspond to not being to find *any* path
                    // for a particular queried field,
                    // which means we have no plan for the overall query.
                    // Now, this shouldn't happen in practice if composition validation
                    // has been run successfully (and is not buggy),
                    // since the goal of composition validation
                    // is exactly to ensure we can never run into this path.
                    // In any case, we will throw later if that happens,
                    // but let's just return the proper result here, which is no plan at all.
                    None
                } else {
                    Some(product * branch.0.len())
                }
            })
            .unwrap_or(0);
        // debug!("Query has {plan_count} possible plans");

        let max_evaluated_plans =
            u32::from(self.parameters.config.debug.max_evaluated_plans) as usize;
        loop {
            // Note that if `self.closed_branches[0]` is our only branch, it's fine,
            // we'll continue to remove options from it (but that is beyond unlikely).
            let first_branch_len = self.closed_branches[0].0.len();
            if plan_count <= max_evaluated_plans || first_branch_len <= 1 {
                break;
            }
            Self::prune_and_reorder_first_branch(&mut self.closed_branches);
            plan_count -= plan_count / first_branch_len;

            // debug!("Reduced plans to consider to {plan_count} plans");
        }
    }

    /// Removes the right-most option of the first branch and moves that branch to its new place
    /// to keep them sorted by decreasing number of options.
    /// Assumes that branches were already sorted that way, and that there is at least one branch.
    ///
    /// This takes a generic parameter instead of `&mut self` for unit-testing.
    fn prune_and_reorder_first_branch(closed_branches: &mut [impl ClosedBranchLike]) {
        let (first_branch, rest) = closed_branches.split_first_mut().unwrap();
        let first_branch_previous_len = first_branch.len();
        first_branch.pop();
        let to_jump_over = rest
            .iter()
            .take_while(|branch| branch.len() == first_branch_previous_len)
            .count();
        if to_jump_over == 0 {
            // No other branch has as many options as `closed_branches[0]` did,
            // so removing one option still left `closed_branches` sorted.
        } else {
            // `closed_branches` now looks like this:
            //
            // | index            | number of options in branch      |
            // | ---------------- | -------------------------------- |
            // | 0                | first_branch_previous_len - 1    |
            // | 1                | first_branch_previous_len        |
            // | …                | first_branch_previous_len        |
            // | to_jump_over     | first_branch_previous_len        |
            // | to_jump_over + 1 | <= first_branch_previous_len - 1 |
            // | …                | <= first_branch_previous_len - 1 |
            //
            // The range `closed_branches[1 ..= to_jump_over]` is branches
            // that all have the same number of options, so they can be in any relative order.

            closed_branches.swap(0, to_jump_over)

            // `closed_branches` now looks like this, which is correctly sorted:
            //
            // | index            | number of options in branch      |
            // | ---------------- | -------------------------------- |
            // | 0                | first_branch_previous_len        |
            // | 1                | first_branch_previous_len        |
            // | …                | first_branch_previous_len        |
            // | to_jump_over     | first_branch_previous_len - 1    |
            // | to_jump_over + 1 | <= first_branch_previous_len - 1 |
            // | …                | <= first_branch_previous_len - 1 |
        }
    }

    pub(crate) fn new_dependency_graph(&self) -> FetchDependencyGraph {
        let root_type = if self.is_top_level && self.has_defers {
            self.parameters
                .supergraph_schema
                .schema()
                .root_operation(self.root_kind.into())
                .cloned()
                // A root operation type has to be an object type
                .map(|type_name| ObjectTypeDefinitionPosition { type_name }.into())
        } else {
            None
        };
        FetchDependencyGraph::new(
            self.parameters.supergraph_schema.clone(),
            self.parameters.federated_query_graph.clone(),
            root_type,
            self.starting_id_generation,
        )
    }

    fn updated_dependency_graph(
        &self,
        dependency_graph: &mut FetchDependencyGraph,
        path_tree: &OpPathTree,
    ) -> Result<(), FederationError> {
        let is_root_path_tree = matches!(
            path_tree.graph.node_weight(path_tree.node)?.type_,
            QueryGraphNodeType::FederatedRootType(_)
        );
        if is_root_path_tree {
            // The root of the pathTree is one of the "fake" root of the subgraphs graph,
            // which belongs to no subgraph but points to each ones.
            // So we "unpack" the first level of the tree to find out our top level groups
            // (and initialize our stack).
            // Note that we can safely ignore the triggers of that first level
            // as it will all be free transition, and we know we cannot have conditions.
            for child in &path_tree.childs {
                let edge = child.edge.expect("The root edge should not be None");
                let (_source_node, target_node) = path_tree.graph.edge_endpoints(edge)?;
                let target_node = path_tree.graph.node_weight(target_node)?;
                let subgraph_name = &target_node.source;
                let root_type = match &target_node.type_ {
                    QueryGraphNodeType::SchemaType(OutputTypeDefinitionPosition::Object(
                        object,
                    )) => object.clone().into(),
                    ty => {
                        return Err(FederationError::internal(format!(
                            "expected an object type for the root of a subgraph, found {ty}"
                        )))
                    }
                };
                let fetch_dependency_node = dependency_graph.get_or_create_root_node(
                    subgraph_name,
                    self.root_kind,
                    root_type,
                )?;
                compute_nodes_for_tree(
                    dependency_graph,
                    &child.tree,
                    fetch_dependency_node,
                    Default::default(),
                    Default::default(),
                    &Default::default(),
                )?;
            }
        } else {
            let query_graph_node = path_tree.graph.node_weight(path_tree.node)?;
            let subgraph_name = &query_graph_node.source;
            let root_type = match &query_graph_node.type_ {
                QueryGraphNodeType::SchemaType(position) => position.clone().try_into()?,
                QueryGraphNodeType::FederatedRootType(_) => {
                    return Err(FederationError::internal(
                        "unexpected FederatedRootType not at the start of an OpPathTree",
                    ))
                }
            };
            let fetch_dependency_node = dependency_graph.get_or_create_root_node(
                subgraph_name,
                self.root_kind,
                root_type,
            )?;
            compute_nodes_for_tree(
                dependency_graph,
                path_tree,
                fetch_dependency_node,
                Default::default(),
                Default::default(),
                &Default::default(),
            )?;
        }
        Ok(())
    }
}

trait ClosedBranchLike {
    fn len(&self) -> usize;
    fn pop(&mut self);
}

impl ClosedBranchLike for ClosedBranch {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn pop(&mut self) {
        self.0.pop();
    }
}

#[cfg(test)]
impl ClosedBranchLike for String {
    fn len(&self) -> usize {
        self.len()
    }

    fn pop(&mut self) {
        self.pop();
    }
}

#[test]
fn test_prune_and_reorder_first_branch() {
    #[track_caller]
    fn assert(branches: &[&str], expected: &[&str]) {
        let mut branches: Vec<_> = branches.iter().map(|s| s.to_string()).collect();
        QueryPlanningTraversal::prune_and_reorder_first_branch(&mut branches);
        assert_eq!(branches, expected)
    }
    // Either the first branch had strictly more options than the second,
    // so it is still at its correct potition after removing one option…
    assert(
        &["abcdE", "fgh", "ijk", "lmn", "op"],
        &["abcd", "fgh", "ijk", "lmn", "op"],
    );
    assert(
        &["abcD", "fgh", "ijk", "lmn", "op"],
        &["abc", "fgh", "ijk", "lmn", "op"],
    );
    assert(&["abcD", "fgh"], &["abc", "fgh"]);
    assert(&["abcD"], &["abc"]);

    // … or, removing exactly one option from the first branch causes it
    // to now have one less option (in this example: two options)
    // than the second branch (here: three options)
    // There is no other possibility with branches correctly sorted
    // before calling `prune_and_reorder_first_branch`.
    //
    // There may be a run of consecutive branches (here: three branches)
    // with equal number of options (here: three options each).
    // Those branches can be in any relative order.
    // We take advantage of that and swap the now-incorrectly-placed first branch
    // with the last of this run:
    assert(
        &["abC", "fgh", "ijk", "lmn", "op"],
        &["lmn", "fgh", "ijk", "ab", "op"],
    );
    assert(&["abC", "fgh", "ijk", "lmn"], &["lmn", "fgh", "ijk", "ab"]);
    // The "run" can be a single branch:
    assert(&["abC", "lmn", "op"], &["lmn", "ab", "op"]);
    assert(&["abC", "lmn"], &["lmn", "ab"]);
}
