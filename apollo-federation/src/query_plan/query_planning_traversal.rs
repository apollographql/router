use std::sync::Arc;

use apollo_compiler::collections::IndexSet;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;
use serde::Serialize;
use tracing::trace;

use super::fetch_dependency_graph::FetchIdGenerator;
use crate::error::FederationError;
use crate::operation::Operation;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::query_graph::condition_resolver::ConditionResolution;
use crate::query_graph::condition_resolver::ConditionResolutionCacheResult;
use crate::query_graph::condition_resolver::ConditionResolver;
use crate::query_graph::condition_resolver::ConditionResolverCache;
use crate::query_graph::graph_path::create_initial_options;
use crate::query_graph::graph_path::ClosedBranch;
use crate::query_graph::graph_path::ClosedPath;
use crate::query_graph::graph_path::ExcludedConditions;
use crate::query_graph::graph_path::ExcludedDestinations;
use crate::query_graph::graph_path::OpGraphPath;
use crate::query_graph::graph_path::OpGraphPathContext;
use crate::query_graph::graph_path::OpPathElement;
use crate::query_graph::graph_path::OpenBranch;
use crate::query_graph::graph_path::SimultaneousPaths;
use crate::query_graph::graph_path::SimultaneousPathsWithLazyIndirectPaths;
use crate::query_graph::path_tree::OpPathTree;
use crate::query_graph::QueryGraph;
use crate::query_graph::QueryGraphNodeType;
use crate::query_plan::fetch_dependency_graph::compute_nodes_for_tree;
use crate::query_plan::fetch_dependency_graph::FetchDependencyGraph;
use crate::query_plan::fetch_dependency_graph::FetchDependencyGraphNodePath;
use crate::query_plan::fetch_dependency_graph_processor::FetchDependencyGraphProcessor;
use crate::query_plan::fetch_dependency_graph_processor::FetchDependencyGraphToCostProcessor;
use crate::query_plan::generate::generate_all_plans_and_find_best;
use crate::query_plan::generate::PlanBuilder;
use crate::query_plan::query_planner::compute_root_fetch_groups;
use crate::query_plan::query_planner::EnabledOverrideConditions;
use crate::query_plan::query_planner::QueryPlannerConfig;
use crate::query_plan::query_planner::QueryPlanningStatistics;
use crate::query_plan::QueryPlanCost;
use crate::schema::position::AbstractTypeDefinitionPosition;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::schema::ValidFederationSchema;
use crate::utils::logging::snapshot;

// PORT_NOTE: Named `PlanningParameters` in the JS codebase, but there was no particular reason to
// leave out to the `Query` prefix, so it's been added for consistency. Similar to `GraphPath`, we
// don't have a distinguished type for when the head is a root vertex, so we instead check this at
// runtime (introducing the new field `head_must_be_root`).
// NOTE: `head_must_be_root` can be deduced from the `head` node's type, so we might be able to
//       remove it.
pub(crate) struct QueryPlanningParameters<'a> {
    /// The supergraph schema that generated the federated query graph.
    pub(crate) supergraph_schema: ValidFederationSchema,
    /// The federated query graph used for query planning.
    pub(crate) federated_query_graph: Arc<QueryGraph>,
    /// The operation to be query planned.
    pub(crate) operation: Arc<Operation>,
    pub(crate) fetch_id_generator: Arc<FetchIdGenerator>,
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
    pub(crate) statistics: &'a QueryPlanningStatistics,
    pub(crate) override_conditions: EnabledOverrideConditions,
}

pub(crate) struct QueryPlanningTraversal<'a, 'b> {
    /// The parameters given to query planning.
    parameters: &'a QueryPlanningParameters<'b>,
    /// The root kind of the operation.
    root_kind: SchemaRootDefinitionKind,
    /// True if query planner `@defer` support is enabled and the operation contains some `@defer`
    /// application.
    has_defers: bool,
    /// A handle to the sole generator of fetch IDs. While planning an operation, only one of
    /// generator can be used.
    id_generator: Arc<FetchIdGenerator>,
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
    // TODO(@goto-bus-stop): FED-164: can we remove this? `find_best_plan` consumes `self` and returns the
    // best plan, so it should not be necessary to store it.
    best_plan: Option<BestQueryPlanInfo>,
    /// The cache for condition resolution.
    // PORT_NOTE: This is different from JS version. See `ConditionResolver` trait implementation below.
    resolver_cache: ConditionResolverCache,
}

#[derive(Debug, Serialize)]
struct OpenBranchAndSelections {
    /// The options for this open branch.
    open_branch: OpenBranch,
    /// A stack of the remaining selections to plan from the node this open branch ends on.
    selections: Vec<Selection>,
}

struct PlanInfo {
    fetch_dependency_graph: FetchDependencyGraph,
    path_tree: Arc<OpPathTree>,
}

impl std::fmt::Debug for PlanInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.path_tree, f)
    }
}

#[derive(Serialize)]
pub(crate) struct BestQueryPlanInfo {
    /// The fetch dependency graph for this query plan.
    pub(crate) fetch_dependency_graph: FetchDependencyGraph,
    /// The path tree for the closed branch options chosen for this query plan.
    pub(crate) path_tree: Arc<OpPathTree>,
    /// The cost of this query plan.
    pub(crate) cost: QueryPlanCost,
}

impl BestQueryPlanInfo {
    // PORT_NOTE: The equivalent of `createEmptyPlan` in the JS codebase.
    pub(crate) fn empty(parameters: &QueryPlanningParameters) -> Self {
        Self {
            fetch_dependency_graph: FetchDependencyGraph::new(
                parameters.supergraph_schema.clone(),
                parameters.federated_query_graph.clone(),
                None,
                parameters.fetch_id_generator.clone(),
            ),
            path_tree: OpPathTree::new(parameters.federated_query_graph.clone(), parameters.head)
                .into(),
            cost: Default::default(),
        }
    }
}

impl<'a: 'b, 'b> QueryPlanningTraversal<'a, 'b> {
    #[cfg_attr(
        feature = "snapshot_tracing",
        tracing::instrument(level = "trace", skip_all, name = "QueryPlanningTraversal::new")
    )]
    pub(crate) fn new(
        // TODO(@goto-bus-stop): This probably needs a mutable reference for some of the
        // yet-unimplemented methods, and storing a mutable ref in `Self` here smells bad.
        // The ownership of `QueryPlanningParameters` is awkward and should probably be
        // refactored.
        parameters: &'a QueryPlanningParameters,
        selection_set: SelectionSet,
        has_defers: bool,
        root_kind: SchemaRootDefinitionKind,
        cost_processor: FetchDependencyGraphToCostProcessor,
    ) -> Result<Self, FederationError> {
        Self::new_inner(
            parameters,
            selection_set,
            has_defers,
            parameters.fetch_id_generator.clone(),
            root_kind,
            cost_processor,
            Default::default(),
            Default::default(),
            Default::default(),
        )
    }

    // Many arguments is okay for a private constructor function.
    #[allow(clippy::too_many_arguments)]
    #[cfg_attr(
        feature = "snapshot_tracing",
        tracing::instrument(level = "trace", skip_all, name = "QueryPlanningTraversal::new_inner")
    )]
    fn new_inner(
        parameters: &'a QueryPlanningParameters,
        selection_set: SelectionSet,
        has_defers: bool,
        id_generator: Arc<FetchIdGenerator>,
        root_kind: SchemaRootDefinitionKind,
        cost_processor: FetchDependencyGraphToCostProcessor,
        initial_context: OpGraphPathContext,
        excluded_destinations: ExcludedDestinations,
        excluded_conditions: ExcludedConditions,
    ) -> Result<Self, FederationError> {
        let is_top_level = parameters.head_must_be_root;

        fn map_options_to_selections(
            selection_set: SelectionSet,
            options: Vec<SimultaneousPathsWithLazyIndirectPaths>,
        ) -> Vec<OpenBranchAndSelections> {
            let open_branch = OpenBranch(options);
            let selections = selection_set.selections.values().cloned().rev().collect();
            vec![OpenBranchAndSelections {
                open_branch,
                selections,
            }]
        }

        let initial_path = OpGraphPath::new(
            Arc::clone(&parameters.federated_query_graph),
            parameters.head,
        )
        .unwrap();

        // In JS this is done *inside* create_initial_options, which would require awareness of the
        // query graph.
        let tail = parameters
            .federated_query_graph
            .node_weight(initial_path.tail)?;

        // Two-step initialization: initializing open_branches requires a condition resolver,
        // which `QueryPlanningTraversal` is.
        let mut traversal = Self {
            parameters,
            root_kind,
            has_defers,
            id_generator,
            cost_processor,
            is_top_level,
            open_branches: Default::default(),
            closed_branches: Default::default(),
            best_plan: None,
            resolver_cache: ConditionResolverCache::new(),
        };

        let initial_options = create_initial_options(
            initial_path,
            &tail.type_,
            initial_context,
            &mut traversal,
            excluded_destinations,
            excluded_conditions,
            &parameters.override_conditions,
        )?;

        traversal.open_branches = map_options_to_selections(selection_set, initial_options);

        Ok(traversal)
    }

    // PORT_NOTE: In JS, the traversal is still usable after finding the best plan. Here we consume
    // the struct so we do not need to return a reference, which is very unergonomic.
    #[cfg_attr(
        feature = "snapshot_tracing",
        tracing::instrument(
            level = "trace",
            skip_all,
            name = "QueryPlanningTraversal::find_best_plan"
        )
    )]
    pub(crate) fn find_best_plan(mut self) -> Result<Option<BestQueryPlanInfo>, FederationError> {
        self.find_best_plan_inner()?;
        Ok(self.best_plan)
    }

    #[cfg_attr(
        feature = "snapshot_tracing",
        tracing::instrument(
            level = "trace",
            skip_all,
            name = "QueryPlanningTraversal::find_best_plan_inner"
        )
    )]
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
                trace!("Planning termianted!");
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
    #[cfg_attr(
        feature = "snapshot_tracing",
        tracing::instrument(
            level = "trace",
            skip_all,
            name = "QueryPlanningTraversal::handle_open_branch"
        )
    )]
    fn handle_open_branch(
        &mut self,
        selection: &Selection,
        options: &mut Vec<SimultaneousPathsWithLazyIndirectPaths>,
    ) -> Result<(bool, Option<OpenBranchAndSelections>), FederationError> {
        let operation_element = selection.element()?;
        let mut new_options = vec![];
        let mut no_followups: bool = false;

        snapshot!(name = "Options", options, "options");

        snapshot!(
            "OperationElement",
            operation_element.to_string(),
            "operation_element"
        );

        for option in options.iter_mut() {
            let followups_for_option = option.advance_with_operation_element(
                self.parameters.supergraph_schema.clone(),
                &operation_element,
                /*resolver*/ self,
                &self.parameters.override_conditions,
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

        snapshot!(new_options, "new_options");

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
                            simultaneous_path.terminate_with_non_requested_typename_field(
                                &self.parameters.override_conditions,
                            )?,
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

        if let Some(selection_set) = selection.selection_set() {
            let mut all_tail_nodes = IndexSet::default();
            for option in &new_options {
                for path in &option.paths.0 {
                    all_tail_nodes.insert(path.tail);
                }
            }
            if self.selection_set_is_fully_local_from_all_nodes(selection_set, &all_tail_nodes)?
                && !selection.has_defer()
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
                        .add_typename_field_for_abstract_types(None)?,
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
        selection: &SelectionSet,
        nodes: &IndexSet<NodeIndex>,
    ) -> Result<bool, FederationError> {
        // To guarantee that the selection is fully local from the provided vertex/type, we must have:
        // - no edge crossing subgraphs from that vertex.
        // - the type must be compositeType (mostly just ensuring the selection make sense).
        // - everything in the selection must be avaiable in the type (which `rebaseOn` essentially validates).
        // - the selection must not "type-cast" into any abstract type that has inconsistent runtimes acrosse subgraphs. The reason for the
        //   later condition is that `selection` is originally a supergraph selection, but that we're looking to apply "as-is" to a subgraph.
        //   But suppose it has a `... on I` where `I` is an interface. Then it's possible that `I` includes "more" types in the supergraph
        //   than in the subgraph, and so we might have to type-explode it. If so, we cannot use the selection "as-is".
        let mut has_inconsistent_abstract_types: Option<bool> = None;
        let mut check_has_inconsistent_runtime_types = || match has_inconsistent_abstract_types {
            Some(has_inconsistent_abstract_types) => {
                Ok::<bool, FederationError>(has_inconsistent_abstract_types)
            }
            None => {
                let check_result = selection.any_element(&mut |element| match element {
                    OpPathElement::InlineFragment(inline_fragment) => {
                        match &inline_fragment.type_condition_position {
                            Some(type_condition) => Ok(self
                                .parameters
                                .abstract_types_with_inconsistent_runtime_types
                                .iter()
                                .any(|ty| ty.type_name() == type_condition.type_name())),
                            None => Ok(false),
                        }
                    }
                    _ => Ok(false),
                })?;
                has_inconsistent_abstract_types = Some(check_result);
                Ok(check_result)
            }
        };
        for node in nodes {
            let n = self.parameters.federated_query_graph.node_weight(*node)?;
            if n.has_reachable_cross_subgraph_edges {
                return Ok(false);
            }
            let parent_ty = match &n.type_ {
                QueryGraphNodeType::SchemaType(ty) => {
                    match CompositeTypeDefinitionPosition::try_from(ty.clone()) {
                        Ok(ty) => ty,
                        _ => return Ok(false),
                    }
                }
                QueryGraphNodeType::FederatedRootType(_) => return Ok(false),
            };
            let schema = self
                .parameters
                .federated_query_graph
                .schema_by_source(&n.source)?;
            if !selection.can_rebase_on(&parent_ty, schema)? {
                return Ok(false);
            }
            if check_has_inconsistent_runtime_types()? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn cost(
        &mut self,
        dependency_graph: &mut FetchDependencyGraph,
    ) -> Result<QueryPlanCost, FederationError> {
        let (main, deferred) = dependency_graph.process(self.cost_processor, self.root_kind)?;
        if deferred.is_empty() {
            Ok(main)
        } else {
            let Some(primary_selection) =
                dependency_graph.defer_tracking.primary_selection.as_ref()
            else {
                // PORT_NOTE: The JS version unwraps here.
                return Err(FederationError::internal(
                    "Primary selection not set in fetch dependency graph",
                ));
            };
            self.cost_processor
                .reduce_defer(main, primary_selection, deferred)
        }
    }

    #[cfg_attr(
        feature = "snapshot_tracing",
        tracing::instrument(
            level = "trace",
            skip_all,
            name = "QueryPlanningTraversal::compute_best_plan_from_closed_branches"
        )
    )]
    fn compute_best_plan_from_closed_branches(&mut self) -> Result<(), FederationError> {
        snapshot!(
            name = "ClosedBranches",
            self.closed_branches,
            "closed_branches"
        );

        if self.closed_branches.is_empty() {
            return Ok(());
        }
        self.sort_options_in_closed_branches()?;
        self.reduce_options_if_needed();

        snapshot!(
            name = "ClosedBranches",
            self.closed_branches,
            "closed_branches_after_reduce"
        );

        // debug log
        // self.closed_branches
        //     .iter()
        //     .enumerate()
        //     .for_each(|(i, branch)| {
        //         println!("{i}:");
        //         branch.0.iter().for_each(|path| {
        //             println!("  - {path}");
        //         });
        //     });

        // Note that usually we'll have a majority of branches with just one option. We can group them in
        // a PathTree first with no fuss. When then need to do a cartesian product between this created
        // tree an other branches however to build the possible plans and chose.

        // find the index of the branch with only one path in self.closed_branches.
        let sole_path_branch_index = self
            .closed_branches
            .iter()
            .position(|branch| branch.0.len() == 1)
            .unwrap_or(self.closed_branches.len());
        // first_group: the first half of branches that have multiple choices.
        // second_group: the second half starting with a branch that has only one choice.
        let (first_group, second_group) = self.closed_branches.split_at(sole_path_branch_index);

        let initial_tree;
        snapshot!("FetchDependencyGraph", "", "Generating initial dep graph");
        let mut initial_dependency_graph = self.new_dependency_graph();
        let federated_query_graph = &self.parameters.federated_query_graph;
        let root = &self.parameters.head;
        if second_group.is_empty() {
            // Unfortunately, all branches have more than one choices.
            initial_tree = OpPathTree::new(federated_query_graph.clone(), *root);
        } else {
            // Build a tree with the second group's paths.
            let single_choice_branches: Vec<_> = second_group
                .iter()
                .flat_map(|b| &b.0)
                .flat_map(|cp| cp.flatten())
                .collect();
            initial_tree = OpPathTree::from_op_paths(
                federated_query_graph.clone(),
                *root,
                &single_choice_branches,
            )?;
            self.updated_dependency_graph(
                &mut initial_dependency_graph,
                &initial_tree,
                self.parameters.config.type_conditioned_fetching,
            )?;
            snapshot!(
                initial_dependency_graph,
                "Updated dep graph with initial tree"
            );
            if first_group.is_empty() {
                // Well, we have the only possible plan; it's also the best.
                let cost = self.cost(&mut initial_dependency_graph)?;
                self.best_plan = BestQueryPlanInfo {
                    fetch_dependency_graph: initial_dependency_graph,
                    path_tree: initial_tree.into(),
                    cost,
                }
                .into();

                snapshot!(self.best_plan, "best_plan");

                return Ok(());
            }
        }

        // Build trees from the first group
        let other_trees: Vec<Vec<Option<Arc<OpPathTree>>>> = first_group
            .iter()
            .map(|b| {
                b.0.iter()
                    .map(|opt| {
                        OpPathTree::from_op_paths(
                            federated_query_graph.clone(),
                            *root,
                            &Vec::from_iter(opt.flatten()),
                        )
                        .ok()
                        .map(Arc::new)
                    })
                    .collect()
            })
            .collect();

        let (best, cost) = generate_all_plans_and_find_best(
            PlanInfo {
                fetch_dependency_graph: initial_dependency_graph,
                path_tree: Arc::new(initial_tree),
            },
            other_trees,
            /*plan_builder*/ self,
        )?;
        self.best_plan = BestQueryPlanInfo {
            fetch_dependency_graph: best.fetch_dependency_graph,
            path_tree: best.path_tree,
            cost,
        }
        .into();

        snapshot!(self.best_plan, "best_plan");
        Ok(())
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

        /// Returns usize::MAX for integer overflow
        fn product_of_closed_branches_len(closed_branches: &[ClosedBranch]) -> usize {
            let mut product: usize = 1;
            for branch in closed_branches {
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
                    return 0;
                } else {
                    let Some(new_product) = product.checked_mul(branch.0.len()) else {
                        return usize::MAX;
                    };
                    product = new_product
                }
            }
            product
        }

        let mut plan_count = product_of_closed_branches_len(&self.closed_branches);
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
            if plan_count != usize::MAX {
                // We had `old_plan_count == first_branch_len * rest` and
                // reduced `first_branch_len` by 1, so the new count is:
                //
                // (first_branch_len - 1) * rest
                // = first_branch_len * rest - rest
                // = (first_branch_len * rest) - (first_branch_len * rest) / first_branch_len
                // = old_plan_count - old_plan_count / first_branch_len
                plan_count -= plan_count / first_branch_len;
            } else {
                // Previous count had overflowed, so recompute the reduced one from scratch
                plan_count = product_of_closed_branches_len(&self.closed_branches)
            }

            // debug!("Reduced plans to consider to {plan_count} plans");
        }

        if self.is_top_level {
            let evaluated = &self.parameters.statistics.evaluated_plan_count;
            evaluated.set(evaluated.get() + plan_count);
        } else {
            // We're resolving a sub-plan for an edge condition,
            // and we don't want to count those as "evaluated plans".
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
            self.id_generator.clone(),
        )
    }

    #[cfg_attr(
        feature = "snapshot_tracing",
        tracing::instrument(
            level = "trace",
            skip_all,
            name = "QueryPlanningTraversal::updated_dependency_graph"
        )
    )]
    fn updated_dependency_graph(
        &self,
        dependency_graph: &mut FetchDependencyGraph,
        path_tree: &OpPathTree,
        type_conditioned_fetching_enabled: bool,
    ) -> Result<(), FederationError> {
        let is_root_path_tree = matches!(
            path_tree.graph.node_weight(path_tree.node)?.type_,
            QueryGraphNodeType::FederatedRootType(_)
        );
        if is_root_path_tree {
            compute_root_fetch_groups(
                self.root_kind,
                dependency_graph,
                path_tree,
                type_conditioned_fetching_enabled,
            )?;
        } else {
            let query_graph_node = path_tree.graph.node_weight(path_tree.node)?;
            let subgraph_name = &query_graph_node.source;
            let root_type: CompositeTypeDefinitionPosition = match &query_graph_node.type_ {
                QueryGraphNodeType::SchemaType(position) => position.clone().try_into()?,
                QueryGraphNodeType::FederatedRootType(_) => {
                    return Err(FederationError::internal(
                        "unexpected FederatedRootType not at the start of an OpPathTree",
                    ));
                }
            };
            let fetch_dependency_node = dependency_graph.get_or_create_root_node(
                subgraph_name,
                self.root_kind,
                root_type.clone(),
            )?;
            compute_nodes_for_tree(
                dependency_graph,
                path_tree,
                fetch_dependency_node,
                FetchDependencyGraphNodePath::new(
                    dependency_graph.supergraph_schema.clone(),
                    self.parameters.config.type_conditioned_fetching,
                    root_type,
                )?,
                Default::default(),
                &Default::default(),
            )?;
        }

        snapshot!(dependency_graph, "updated_dependency_graph");
        Ok(())
    }

    #[cfg_attr(
        feature = "snapshot_tracing",
        tracing::instrument(
            level = "trace",
            skip_all,
            name = "QueryPlanningTraversal::resolve_condition_plan"
        )
    )]
    fn resolve_condition_plan(
        &self,
        edge: EdgeIndex,
        context: &OpGraphPathContext,
        excluded_destinations: &ExcludedDestinations,
        excluded_conditions: &ExcludedConditions,
    ) -> Result<ConditionResolution, FederationError> {
        let graph = &self.parameters.federated_query_graph;
        let head = graph.edge_endpoints(edge)?.0;
        // Note: `QueryPlanningTraversal::resolve` method asserts that the edge has conditions before
        //       calling this method.
        let edge_conditions = graph
            .edge_weight(edge)?
            .conditions
            .as_ref()
            .unwrap()
            .as_ref();
        let parameters = QueryPlanningParameters {
            head,
            head_must_be_root: graph.node_weight(head)?.is_root_node(),
            // otherwise, the same as self.parameters
            // TODO: Some fields are deep-cloned here. We might want to revisit how they should be defined.
            supergraph_schema: self.parameters.supergraph_schema.clone(),
            federated_query_graph: graph.clone(),
            operation: self.parameters.operation.clone(),
            abstract_types_with_inconsistent_runtime_types: self
                .parameters
                .abstract_types_with_inconsistent_runtime_types
                .clone(),
            config: self.parameters.config.clone(),
            statistics: self.parameters.statistics,
            override_conditions: self.parameters.override_conditions.clone(),
            fetch_id_generator: self.parameters.fetch_id_generator.clone(),
        };
        let best_plan_opt = QueryPlanningTraversal::new_inner(
            &parameters,
            edge_conditions.clone(),
            self.has_defers,
            self.id_generator.clone(),
            self.root_kind,
            self.cost_processor,
            context.clone(),
            excluded_destinations.clone(),
            excluded_conditions.add_item(edge_conditions),
        )?
        .find_best_plan()?;
        match best_plan_opt {
            Some(best_plan) => Ok(ConditionResolution::Satisfied {
                cost: best_plan.cost,
                path_tree: Some(best_plan.path_tree),
            }),
            None => Ok(ConditionResolution::unsatisfied_conditions()),
        }
    }
}

impl<'a: 'b, 'b> PlanBuilder<PlanInfo, Arc<OpPathTree>> for QueryPlanningTraversal<'a, 'b> {
    fn add_to_plan(
        &mut self,
        plan_info: &PlanInfo,
        tree: Arc<OpPathTree>,
    ) -> Result<PlanInfo, FederationError> {
        let mut updated_graph = plan_info.fetch_dependency_graph.clone();
        self.updated_dependency_graph(
            &mut updated_graph,
            &tree,
            self.parameters.config.type_conditioned_fetching,
        )
        .map(|_| PlanInfo {
            fetch_dependency_graph: updated_graph,
            path_tree: plan_info.path_tree.merge(&tree),
        })
    }

    fn compute_plan_cost(
        &mut self,
        plan_info: &mut PlanInfo,
    ) -> Result<QueryPlanCost, FederationError> {
        self.cost(&mut plan_info.fetch_dependency_graph)
    }

    fn on_plan_generated(
        &self,
        _plan_info: &PlanInfo,
        _cost: QueryPlanCost,
        _prev_cost: Option<QueryPlanCost>,
    ) {
        // debug log
        // if prev_cost.is_none() {
        //     print!("Computed plan with cost {}: {}", cost, plan_tree);
        // } else if cost > prev_cost.unwrap() {
        //     print!(
        //         "Ignoring plan with cost {} (a better plan with cost {} exists): {}",
        //         cost,
        //         prev_cost.unwrap(),
        //         plan_tree
        //     );
        // } else {
        //     print!(
        //         "Found better with cost {} (previous had cost {}): {}",
        //         cost,
        //         prev_cost.unwrap(),
        //         plan_tree
        //     );
        // }
    }
}

// PORT_NOTE: In JS version, QueryPlanningTraversal has `conditionResolver` field, which
//            is a closure calling `this.resolveConditionPlan` (`this` is captured here).
//            The same would be infeasible to implement in Rust due to the cyclic references.
//            Thus, instead of `condition_resolver` field, QueryPlanningTraversal was made to
//            implement `ConditionResolver` trait along with `resolver_cache` field.
impl<'a> ConditionResolver for QueryPlanningTraversal<'a, '_> {
    /// A query plan resolver for edge conditions that caches the outcome per edge.
    fn resolve(
        &mut self,
        edge: EdgeIndex,
        context: &OpGraphPathContext,
        excluded_destinations: &ExcludedDestinations,
        excluded_conditions: &ExcludedConditions,
    ) -> Result<ConditionResolution, FederationError> {
        // Invariant check: The edge must have conditions.
        let graph = &self.parameters.federated_query_graph;
        let edge_data = graph.edge_weight(edge)?;
        assert!(
            edge_data.conditions.is_some(),
            "Should not have been called for edge without conditions"
        );

        let cache_result =
            self.resolver_cache
                .contains(edge, context, excluded_destinations, excluded_conditions);

        if let ConditionResolutionCacheResult::Hit(cached_resolution) = cache_result {
            return Ok(cached_resolution);
        }

        let resolution =
            self.resolve_condition_plan(edge, context, excluded_destinations, excluded_conditions)?;
        // See if this resolution is eligible to be inserted into the cache.
        if cache_result.is_miss() {
            self.resolver_cache
                .insert(edge, resolution.clone(), excluded_destinations.clone());
        }
        Ok(resolution)
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
