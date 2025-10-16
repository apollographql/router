use apollo_compiler::Name;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::ExtendedType;
use indexmap::map::Entry;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use petgraph::visit::IntoNodeReferences;

use crate::bail;
use crate::ensure;
use crate::error::FederationError;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::query_graph::OverrideCondition;
use crate::query_graph::QueryGraph;
use crate::query_graph::QueryGraphEdgeTransition;
use crate::query_graph::graph_path::operation::OpPathElement;
use crate::query_plan::query_planning_traversal::QueryPlanningTraversal;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::INTROSPECTION_TYPENAME_FIELD_NAME;
use crate::schema::position::ObjectTypeDefinitionPosition;

impl<'a: 'b, 'b> QueryPlanningTraversal<'a, 'b> {
    pub(super) const MAX_NON_LOCAL_SELECTIONS: u64 = 100_000;

    /// This calls `check_non_local_selections_limit_exceeded()` for each of the selections in the
    /// open branches stack; see that function's doc comment for more information.
    ///
    /// To support mutations, we allow indicating the initial subgraph is constrained, in which case
    /// indirect options will be ignored until the first field (similar to query planning).
    pub(super) fn check_non_local_selections_limit_exceeded_at_root(
        &self,
        state: &mut State,
        is_initial_subgraph_constrained: bool,
    ) -> Result<bool, FederationError> {
        for branch in &self.open_branches {
            let tail_nodes = branch
                .open_branch
                .0
                .iter()
                .flat_map(|option| option.paths.0.iter().map(|path| path.tail()))
                .collect::<IndexSet<_>>();
            let tail_nodes_info = self.estimate_nodes_with_indirect_options(
                tail_nodes,
                is_initial_subgraph_constrained,
            )?;

            // Note that top-level selections aren't avoided via fully-local selection set
            // optimization, so we always add them here.
            if Self::update_count(
                branch.selections.len(),
                tail_nodes_info.next_nodes.len(),
                state,
            ) {
                return Ok(true);
            }

            for selection in &branch.selections {
                if let Some(selection_set) = selection.selection_set() {
                    let selection_has_defer = selection.element().has_defer();
                    let is_initial_subgraph_constrained_after_element =
                        is_initial_subgraph_constrained
                            && matches!(selection, Selection::InlineFragment(_));
                    let next_nodes = self.estimate_next_nodes_for_selection(
                        &selection.element(),
                        &tail_nodes_info,
                        state,
                        is_initial_subgraph_constrained_after_element,
                    )?;
                    if self.check_non_local_selections_limit_exceeded(
                        selection_set,
                        &next_nodes,
                        selection_has_defer,
                        state,
                        is_initial_subgraph_constrained_after_element,
                    )? {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// When recursing through a selection set to generate options from each element, there is an
    /// optimization that allows us to avoid option exploration if a selection set is "fully local"
    /// from all the possible nodes we could be at in the query graph.
    ///
    /// This function computes an approximate upper bound on the number of selections in a selection
    /// set that wouldn't be avoided by such an optimization (i.e. the "non-local" selections), and
    /// adds it to the given count in the state. Note that the count for a given selection set is
    /// scaled by an approximate upper bound on the possible number of tail nodes for paths ending
    /// at that selection set. If at any point, the count exceeds `Self::MAX_NON_LOCAL_SELECTIONS`,
    /// then this function will return `true`.
    ///
    /// This function's code is closely related to `selection_set_is_fully_local_from_all_nodes()`
    /// (which implements the aforementioned optimization). However, when it comes to traversing the
    /// query graph, we generally ignore the effects of edge pre-conditions and other optimizations
    /// to option generation for efficiency's sake, giving us an upper bound since the extra nodes
    /// may fail some of the checks (e.g. the selection set may not rebase on them).
    ///
    /// Note that this function takes in whether the parent selection of the selection set has
    /// @defer, as that affects whether the optimization is disabled for that selection set.
    ///
    /// To support mutations, we allow indicating the initial subgraph is constrained, in which case
    /// indirect options will be ignored until the first field (similar to query planning).
    fn check_non_local_selections_limit_exceeded(
        &self,
        selection_set: &SelectionSet,
        parent_nodes: &NextNodesInfo,
        parent_selection_has_defer: bool,
        state: &mut State,
        is_initial_subgraph_constrained: bool,
    ) -> Result<bool, FederationError> {
        // Compute whether the selection set is non-local, and if so, add its selections to the
        // count. Any of the following causes the selection set to be non-local.
        // 1. The selection set's nodes having at least one reachable cross-subgraph edge.
        // 2. The parent selection having @defer.
        // 3. Any selection in the selection set having @defer.
        // 4. Any selection in the selection set being an inline fragment whose type condition
        //    has inconsistent runtime types across subgraphs.
        // 5. Any selection in the selection set being unable to be rebased on the selection
        //    set's nodes.
        // 6. Any nested selection sets causing the count to be incremented.
        let mut selection_set_is_non_local = parent_nodes
            .next_nodes_have_reachable_cross_subgraph_edges
            || parent_selection_has_defer;
        for selection in selection_set.selections.values() {
            let element = selection.element();
            let selection_has_defer = element.has_defer();
            let selection_has_inconsistent_runtime_types =
                if let OpPathElement::InlineFragment(inline_fragment) = element {
                    inline_fragment
                        .type_condition_position
                        .map(|type_condition_pos| {
                            self.parameters
                                .abstract_types_with_inconsistent_runtime_types
                                .contains(type_condition_pos.type_name())
                        })
                        .unwrap_or_default()
                } else {
                    false
                };

            let old_count = state.count;
            if let Some(selection_set) = selection.selection_set() {
                let is_initial_subgraph_constrained_after_element = is_initial_subgraph_constrained
                    && matches!(selection, Selection::InlineFragment(_));
                let next_nodes = self.estimate_next_nodes_for_selection(
                    &selection.element(),
                    parent_nodes,
                    state,
                    is_initial_subgraph_constrained_after_element,
                )?;
                if self.check_non_local_selections_limit_exceeded(
                    selection_set,
                    &next_nodes,
                    selection_has_defer,
                    state,
                    is_initial_subgraph_constrained_after_element,
                )? {
                    return Ok(true);
                }
            }

            selection_set_is_non_local = selection_set_is_non_local
                || selection_has_defer
                || selection_has_inconsistent_runtime_types
                || (old_count != state.count);
        }
        // Determine whether the selection can be rebased on all selection set nodes (without
        // indirect options). This is more expensive, so we do this last/only if needed. Note
        // that we were originally calling a slightly modified `can_add_to()` to mimic the logic
        // in `selection_set_is_fully_local_from_all_nodes()`, but this ended up being rather
        // expensive in practice, so an optimized version using precomputation is used below.
        if !selection_set_is_non_local && !parent_nodes.next_nodes.is_empty() {
            let metadata = self
                .parameters
                .federated_query_graph
                .non_local_selection_metadata();
            for selection in selection_set.selections.values() {
                match selection {
                    Selection::Field(field) => {
                        // Note that while the precomputed metadata accounts for @fromContext,
                        // it doesn't account for checking whether the operation field's parent
                        // type either matches the subgraph schema's parent type name or is an
                        // interface type. Given current composition rules, this should always
                        // be the case when rebasing supergraph/API schema queries onto one of
                        // its subgraph schema, so we avoid the check here for performance.
                        let Some(rebaseable_parent_nodes) = metadata
                            .fields_to_rebaseable_parent_nodes
                            .get(field.field.name())
                        else {
                            selection_set_is_non_local = true;
                            break;
                        };
                        if !parent_nodes.next_nodes.is_subset(rebaseable_parent_nodes) {
                            selection_set_is_non_local = true;
                            break;
                        }
                    }
                    Selection::InlineFragment(inline_fragment) => {
                        let Some(type_condition_pos) =
                            &inline_fragment.inline_fragment.type_condition_position
                        else {
                            // Inline fragments without type conditions can always be rebased.
                            continue;
                        };
                        let Some(rebaseable_parent_nodes) = metadata
                            .inline_fragments_to_rebaseable_parent_nodes
                            .get(type_condition_pos.type_name())
                        else {
                            selection_set_is_non_local = true;
                            break;
                        };
                        if !parent_nodes.next_nodes.is_subset(rebaseable_parent_nodes) {
                            selection_set_is_non_local = true;
                            break;
                        }
                    }
                }
            }
        }
        if selection_set_is_non_local
            && Self::update_count(
                selection_set.selections.len(),
                parent_nodes.next_nodes.len(),
                state,
            )
        {
            return Ok(true);
        }
        Ok(false)
    }

    /// Updates the non-local selection set count in the state, returning true if this causes the
    /// count to exceed `Self::MAX_NON_LOCAL_SELECTIONS`.
    fn update_count(num_selections: usize, num_parent_nodes: usize, state: &mut State) -> bool {
        let Ok(num_selections) = u64::try_from(num_selections) else {
            return true;
        };
        let Ok(num_parent_nodes) = u64::try_from(num_parent_nodes) else {
            return true;
        };
        let Some(additional_count) = num_selections.checked_mul(num_parent_nodes) else {
            return true;
        };
        if let Some(new_count) = state
            .count
            .checked_add(additional_count)
            .take_if(|v| *v <= Self::MAX_NON_LOCAL_SELECTIONS)
        {
            state.count = new_count;
        } else {
            return true;
        };
        false
    }

    /// In `check_non_local_selections_limit_exceeded()`, when handling a given selection for a set
    /// of parent nodes (including indirect options), this function can be used to estimate an
    /// upper bound on the next nodes after taking the selection (also with indirect options).
    ///
    /// To support mutations, we allow indicating the initial subgraph will be constrained after
    /// taking the element, in which case indirect options will be ignored (and caching will be
    /// skipped). This is to ensure that top-level mutation fields are not executed on a different
    /// subgraph than the initial one during query planning.
    fn estimate_next_nodes_for_selection(
        &self,
        element: &OpPathElement,
        parent_nodes: &NextNodesInfo,
        state: &mut State,
        is_initial_subgraph_constrained_after_element: bool,
    ) -> Result<NextNodesInfo, FederationError> {
        if is_initial_subgraph_constrained_after_element {
            if let OpPathElement::InlineFragment(inline_fragment) = element
                && inline_fragment.type_condition_position.is_none()
            {
                return Ok(parent_nodes.clone());
            }

            // When the initial subgraph is constrained, skip caching entirely. Note that caching
            // is not skipped when the initial subgraph is constrained before this element but not
            // after. Because of that, there may be cache entries for remaining nodes that were
            // actually part of a complete digraph, but this is only a slight caching inefficiency
            // and doesn't affect the computation's result.
            ensure!(
                parent_nodes
                    .next_nodes_with_indirect_options
                    .types
                    .is_empty(),
                "Initial subgraph was constrained which indicates no indirect options should be \
                taken, but the parent nodes unexpectedly had a complete digraph which indicates \
                indirect options were taken upstream in the path."
            );
            return self.estimate_next_nodes_for_selection_without_caching(
                element,
                parent_nodes
                    .next_nodes_with_indirect_options
                    .remaining_nodes
                    .iter(),
                true,
            );
        }
        let cache = state
            .next_nodes_cache
            .entry(match element {
                OpPathElement::Field(field) => {
                    SelectionKey::Field(field.field_position.field_name().clone())
                }
                OpPathElement::InlineFragment(inline_fragment) => {
                    let Some(type_condition_pos) = &inline_fragment.type_condition_position else {
                        return Ok(parent_nodes.clone());
                    };
                    SelectionKey::InlineFragment(type_condition_pos.type_name().clone())
                }
            })
            .or_default();
        let mut next_nodes = NextNodesInfo::default();
        for type_name in &parent_nodes.next_nodes_with_indirect_options.types {
            match cache.types_to_next_nodes.entry(type_name.clone()) {
                Entry::Occupied(entry) => next_nodes.extend(entry.get()),
                Entry::Vacant(entry) => {
                    let Some(indirect_options) = self
                        .parameters
                        .federated_query_graph
                        .non_local_selection_metadata()
                        .types_to_indirect_options
                        .get(type_name)
                    else {
                        bail!("Unexpectedly missing node information for cached type.");
                    };
                    let new_next_nodes = self.estimate_next_nodes_for_selection_without_caching(
                        element,
                        indirect_options.same_type_options.iter(),
                        false,
                    )?;
                    next_nodes.extend(entry.insert(new_next_nodes));
                }
            }
        }
        for node in &parent_nodes
            .next_nodes_with_indirect_options
            .remaining_nodes
        {
            match cache.remaining_nodes_to_next_nodes.entry(*node) {
                Entry::Occupied(entry) => next_nodes.extend(entry.get()),
                Entry::Vacant(entry) => {
                    let new_next_nodes = self.estimate_next_nodes_for_selection_without_caching(
                        element,
                        std::iter::once(node),
                        false,
                    )?;
                    next_nodes.extend(entry.insert(new_next_nodes));
                }
            }
        }
        Ok(next_nodes)
    }

    /// Estimate an upper bound on the next nodes after taking the selection on the given parent
    /// nodes. Because we're just trying for an upper bound, we assume we can always take
    /// type-preserving non-collecting transitions, we ignore any conditions on the selection edge,
    /// and we always type-explode. (We do account for override conditions, which are relatively
    /// straightforward.)
    ///
    /// Since we're iterating through next nodes in the process, for efficiency's sake we also
    /// compute whether there are any reachable cross-subgraph edges from the next nodes (without
    /// indirect options). This method assumes that inline fragments have type conditions.
    ///
    /// To support mutations, we allow indicating the initial subgraph will be constrained after
    /// taking the element, in which case indirect options will be ignored. This is to ensure that
    /// top-level mutation fields are not executed on a different subgraph than the initial one
    /// during query planning.
    fn estimate_next_nodes_for_selection_without_caching<'c>(
        &self,
        element: &OpPathElement,
        parent_nodes: impl Iterator<Item = &'c NodeIndex>,
        is_initial_subgraph_constrained_after_element: bool,
    ) -> Result<NextNodesInfo, FederationError> {
        let mut next_nodes = IndexSet::default();
        let nodes_to_object_type_downcasts = &self
            .parameters
            .federated_query_graph
            .non_local_selection_metadata()
            .nodes_to_object_type_downcasts;
        match element {
            OpPathElement::Field(field) => {
                let Some(field_endpoints) = self
                    .parameters
                    .federated_query_graph
                    .non_local_selection_metadata()
                    .fields_to_endpoints
                    .get(field.name())
                else {
                    return Ok(Default::default());
                };
                let mut process_head_node = |node: NodeIndex| {
                    let Some(target) = field_endpoints.get(&node) else {
                        return;
                    };
                    match target {
                        FieldTarget::NonOverride(target) => {
                            next_nodes.insert(*target);
                        }
                        FieldTarget::Override(target, condition) => {
                            if condition.check(&self.parameters.override_conditions) {
                                next_nodes.insert(*target);
                            }
                        }
                    }
                };
                for node in parent_nodes {
                    // As an upper bound for efficiency's sake, we consider both non-type-exploded
                    // and type-exploded options.
                    process_head_node(*node);
                    let Some(object_type_downcasts) = nodes_to_object_type_downcasts.get(node)
                    else {
                        continue;
                    };
                    match object_type_downcasts {
                        ObjectTypeDowncasts::NonInterfaceObject(downcasts) => {
                            for node in downcasts.values() {
                                process_head_node(*node);
                            }
                        }
                        ObjectTypeDowncasts::InterfaceObject(_) => {
                            // Interface object fake downcasts only go back to the
                            // self node, so we ignore them.
                        }
                    }
                }
            }
            OpPathElement::InlineFragment(inline_fragment) => {
                let Some(type_condition_pos) = &inline_fragment.type_condition_position else {
                    bail!("Inline fragment unexpectedly had no type condition")
                };
                let inline_fragment_endpoints = self
                    .parameters
                    .federated_query_graph
                    .non_local_selection_metadata()
                    .inline_fragments_to_endpoints
                    .get(type_condition_pos.type_name());
                // If we end up computing runtime types for the type condition, only do it once.
                let mut possible_runtime_types = None;
                for node in parent_nodes {
                    // We check whether there's already a (maybe fake) downcast edge for the
                    // type condition (note that we've inserted fake downcasts for same-type
                    // type conditions into the metadata).
                    if let Some(next_node) = inline_fragment_endpoints.and_then(|e| e.get(node)) {
                        next_nodes.insert(*next_node);
                        continue;
                    }

                    // If not, then we need to type explode across the possible runtime types
                    // (in the supergraph schema) for the type condition.
                    let Some(downcasts) = nodes_to_object_type_downcasts.get(node) else {
                        continue;
                    };
                    let possible_runtime_types = match &possible_runtime_types {
                        Some(possible_runtime_types) => possible_runtime_types,
                        None => {
                            let type_condition_in_supergraph_pos = self
                                .parameters
                                .supergraph_schema
                                .get_type(type_condition_pos.type_name().clone())?;
                            possible_runtime_types.insert(
                                self.parameters.supergraph_schema.possible_runtime_types(
                                    type_condition_in_supergraph_pos.try_into()?,
                                )?,
                            )
                        }
                    };

                    match downcasts {
                        ObjectTypeDowncasts::NonInterfaceObject(downcasts) => {
                            for (type_name, target_node) in downcasts {
                                if possible_runtime_types.contains(&ObjectTypeDefinitionPosition {
                                    type_name: type_name.clone(),
                                }) {
                                    next_nodes.insert(*target_node);
                                }
                            }
                        }
                        ObjectTypeDowncasts::InterfaceObject(downcasts) => {
                            for type_name in downcasts {
                                if possible_runtime_types.contains(&ObjectTypeDefinitionPosition {
                                    type_name: type_name.clone(),
                                }) {
                                    // Note that interface object fake downcasts are self edges,
                                    // so we're done once we find one.
                                    next_nodes.insert(*node);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }

        self.estimate_nodes_with_indirect_options(
            next_nodes,
            is_initial_subgraph_constrained_after_element,
        )
    }

    /// Estimate the indirect options for the given next nodes, and return the given next nodes
    /// along with `next_nodes_with_indirect_options` which contains these direct and indirect
    /// options. As an upper bound for efficiency's sake, we assume we can take any indirect option
    /// (i.e. ignore any edge conditions).
    ///
    /// Since we're iterating through next nodes in the process, for efficiency's sake we also
    /// compute whether there are any reachable cross-subgraph edges from the next nodes (without
    /// indirect options).
    ///
    /// To support mutations, we allow ignoring indirect options, as we don't want top-level
    /// mutation fields to be executed on a different subgraph than the initial one. In that case,
    /// `next_nodes_with_indirect_options` will not have any `types`, and the given nodes will be
    /// added to `remaining_nodes` (despite them potentially being part of the complete digraph for
    /// their type). This is fine, as caching logic accounts for this accordingly.
    fn estimate_nodes_with_indirect_options(
        &self,
        next_nodes: IndexSet<NodeIndex>,
        ignore_indirect_options: bool,
    ) -> Result<NextNodesInfo, FederationError> {
        let mut next_nodes_info = NextNodesInfo {
            next_nodes,
            ..Default::default()
        };
        for next_node in &next_nodes_info.next_nodes {
            let next_node_weight = self
                .parameters
                .federated_query_graph
                .node_weight(*next_node)?;
            next_nodes_info.next_nodes_have_reachable_cross_subgraph_edges = next_nodes_info
                .next_nodes_have_reachable_cross_subgraph_edges
                || next_node_weight.has_reachable_cross_subgraph_edges;

            // As noted above, we don't want top-level mutation fields to be executed on a different
            // subgraph than the initial one, so we support ignoring indirect options here.
            if ignore_indirect_options {
                next_nodes_info
                    .next_nodes_with_indirect_options
                    .remaining_nodes
                    .insert(*next_node);
                continue;
            }

            let next_node_type_pos: CompositeTypeDefinitionPosition =
                next_node_weight.type_.clone().try_into()?;
            if let Some(options_metadata) = self
                .parameters
                .federated_query_graph
                .non_local_selection_metadata()
                .types_to_indirect_options
                .get(next_node_type_pos.type_name())
            {
                // If there's an entry in `types_to_indirect_options` for the type, then the
                // complete digraph for T is non-empty, so we add its type. If it's our first
                // time seeing this type, we also add any of the complete digraph's interface
                // object options.
                if next_nodes_info
                    .next_nodes_with_indirect_options
                    .types
                    .insert(next_node_type_pos.type_name().clone())
                {
                    next_nodes_info
                        .next_nodes_with_indirect_options
                        .types
                        .extend(options_metadata.interface_object_options.iter().cloned());
                }
                // If the node is a member of the complete digraph, then we don't need to
                // separately add the remaining node.
                if options_metadata.same_type_options.contains(next_node) {
                    continue;
                }
            }
            // We need to add the remaining node, and if it's our first time seeing it, we also
            // add any of its interface object options.
            if next_nodes_info
                .next_nodes_with_indirect_options
                .remaining_nodes
                .insert(*next_node)
                && let Some(options) = self
                    .parameters
                    .federated_query_graph
                    .non_local_selection_metadata()
                    .remaining_nodes_to_interface_object_options
                    .get(next_node)
            {
                next_nodes_info
                    .next_nodes_with_indirect_options
                    .types
                    .extend(options.iter().cloned());
            }
        }

        Ok(next_nodes_info)
    }
}

/// Precompute relevant metadata about the query graph for speeding up the estimation of the
/// count of non-local selections. Note that none of the algorithms used in this function should
/// take any longer algorithmically as the rest of query graph creation (and similarly for
/// query graph memory).
pub(crate) fn precompute_non_local_selection_metadata(
    graph: &QueryGraph,
) -> Result<QueryGraphMetadata, FederationError> {
    let mut nodes_to_interface_object_options: IndexMap<NodeIndex, IndexSet<Name>> =
        Default::default();
    let mut metadata = QueryGraphMetadata::default();

    for edge_ref in graph.graph().edge_references() {
        match &edge_ref.weight().transition {
            QueryGraphEdgeTransition::FieldCollection {
                field_definition_position,
                ..
            } => {
                // We skip selections where the tail is a non-composite type, as we'll never
                // need to estimate the next nodes for such selections.
                if CompositeTypeDefinitionPosition::try_from(
                    graph.node_weight(edge_ref.target())?.type_.clone(),
                )
                .is_err()
                {
                    continue;
                };
                let target = edge_ref
                    .weight()
                    .override_condition
                    .clone()
                    .map(|condition| FieldTarget::Override(edge_ref.target(), condition))
                    .unwrap_or_else(|| FieldTarget::NonOverride(edge_ref.target()));
                metadata
                    .fields_to_endpoints
                    .entry(field_definition_position.field_name().clone())
                    .or_default()
                    .insert(edge_ref.source(), target);
            }
            QueryGraphEdgeTransition::Downcast {
                to_type_position, ..
            } => {
                if to_type_position.is_object_type() {
                    let ObjectTypeDowncasts::NonInterfaceObject(downcasts) = metadata
                        .nodes_to_object_type_downcasts
                        .entry(edge_ref.source())
                        .or_insert_with(|| {
                            ObjectTypeDowncasts::NonInterfaceObject(Default::default())
                        })
                    else {
                        bail!("Unexpectedly found interface object with regular object downcasts")
                    };
                    downcasts.insert(to_type_position.type_name().clone(), edge_ref.target());
                }
                metadata
                    .inline_fragments_to_endpoints
                    .entry(to_type_position.type_name().clone())
                    .or_default()
                    .insert(edge_ref.source(), edge_ref.target());
            }
            QueryGraphEdgeTransition::InterfaceObjectFakeDownCast { to_type_name, .. } => {
                // Note that fake downcasts for interface objects are only created to "fake"
                // object types.
                let ObjectTypeDowncasts::InterfaceObject(downcasts) = metadata
                    .nodes_to_object_type_downcasts
                    .entry(edge_ref.source())
                    .or_insert_with(|| ObjectTypeDowncasts::InterfaceObject(Default::default()))
                else {
                    bail!("Unexpectedly found abstract type with interface object downcasts")
                };
                downcasts.insert(to_type_name.clone());
                metadata
                    .inline_fragments_to_endpoints
                    .entry(to_type_name.clone())
                    .or_default()
                    .insert(edge_ref.source(), edge_ref.target());
            }
            QueryGraphEdgeTransition::KeyResolution
            | QueryGraphEdgeTransition::RootTypeResolution { .. } => {
                let head_type_pos: CompositeTypeDefinitionPosition = graph
                    .node_weight(edge_ref.source())?
                    .type_
                    .clone()
                    .try_into()?;
                let tail_type_pos: CompositeTypeDefinitionPosition = graph
                    .node_weight(edge_ref.target())?
                    .type_
                    .clone()
                    .try_into()?;
                if head_type_pos.type_name() == tail_type_pos.type_name() {
                    // In this case, we have a non-interface-object key resolution edge or a
                    // root type resolution edge. The tail must be part of the complete digraph
                    // for the tail's type, so we record the member.
                    metadata
                        .types_to_indirect_options
                        .entry(tail_type_pos.type_name().clone())
                        .or_default()
                        .same_type_options
                        .insert(edge_ref.target());
                } else {
                    // Otherwise, this must be an interface object key resolution edge. We don't
                    // know the members of the complete digraph for the head's type yet, so we
                    // can't set the metadata yet, and instead store the head to interface
                    // object type mapping in a temporary map.
                    nodes_to_interface_object_options
                        .entry(edge_ref.source())
                        .or_default()
                        .insert(tail_type_pos.type_name().clone());
                }
            }
            QueryGraphEdgeTransition::SubgraphEnteringTransition => {}
        }
    }

    // Now that we've finished computing members of the complete digraphs, we can properly track
    // interface object options.
    for (node, options) in nodes_to_interface_object_options {
        let node_type_pos: CompositeTypeDefinitionPosition =
            graph.node_weight(node)?.type_.clone().try_into()?;
        if let Some(options_metadata) = metadata
            .types_to_indirect_options
            .get_mut(node_type_pos.type_name())
            && options_metadata.same_type_options.contains(&node)
        {
            options_metadata
                .interface_object_options
                .extend(options.into_iter());
            continue;
        }
        metadata
            .remaining_nodes_to_interface_object_options
            .insert(node, options);
    }

    // The interface object options for the complete digraphs are now correct, but we need to
    // subtract these from any interface object options for remaining nodes.
    for (node, options) in metadata
        .remaining_nodes_to_interface_object_options
        .iter_mut()
    {
        let node_type_pos: CompositeTypeDefinitionPosition =
            graph.node_weight(*node)?.type_.clone().try_into()?;
        let Some(IndirectOptionsMetadata {
            interface_object_options,
            ..
        }) = metadata
            .types_to_indirect_options
            .get(node_type_pos.type_name())
        else {
            continue;
        };
        options.retain(|type_name| !interface_object_options.contains(type_name));
    }

    // If this subtraction left any interface object option sets empty, we remove them.
    metadata
        .remaining_nodes_to_interface_object_options
        .retain(|_, options| !options.is_empty());

    // For all composite type nodes, we pretend that there's a self-downcast edge for that type,
    // as this simplifies next node calculation.
    for (node, node_weight) in graph.graph().node_references() {
        let Ok(node_type_pos) =
            CompositeTypeDefinitionPosition::try_from(node_weight.type_.clone())
        else {
            continue;
        };
        metadata
            .inline_fragments_to_endpoints
            .entry(node_type_pos.type_name().clone())
            .or_default()
            .insert(node, node);
        if node_type_pos.is_object_type()
            && !graph
                .schema_by_source(&node_weight.source)?
                .is_interface_object_type(node_type_pos.clone().into())?
        {
            let ObjectTypeDowncasts::NonInterfaceObject(downcasts) = metadata
                .nodes_to_object_type_downcasts
                .entry(node)
                .or_insert_with(|| ObjectTypeDowncasts::NonInterfaceObject(Default::default()))
            else {
                bail!(
                    "Unexpectedly found object type with interface object downcasts in supergraph"
                )
            };
            downcasts.insert(node_type_pos.type_name().clone(), node);
        }
    }

    // For each subgraph schema, we iterate through its composite types, so that we can collect
    // metadata relevant to rebasing.
    for (subgraph_name, subgraph_schema) in graph.subgraph_schemas() {
        // We pass through each composite type, recording whether the field can be rebased on it
        // along with interface implements/union membership relationships.
        let mut fields_to_rebaseable_types: IndexMap<Name, IndexSet<Name>> = Default::default();
        let mut object_types_to_implementing_composite_types: IndexMap<Name, IndexSet<Name>> =
            Default::default();
        let Some(subgraph_metadata) = subgraph_schema.subgraph_metadata() else {
            bail!("Subgraph schema unexpectedly did not have subgraph metadata")
        };
        let from_context_directive_definition_name = &subgraph_metadata
            .federation_spec_definition()
            .from_context_directive_definition(subgraph_schema)?
            .name;
        for (type_name, type_) in &subgraph_schema.schema().types {
            match type_ {
                ExtendedType::Object(type_) => {
                    // Record fields that don't contain @fromContext as being rebaseable (also
                    // including __typename).
                    for (field_name, field_definition) in &type_.fields {
                        if field_definition.arguments.iter().any(|arg_definition| {
                            arg_definition
                                .directives
                                .has(from_context_directive_definition_name)
                        }) {
                            continue;
                        }
                        fields_to_rebaseable_types
                            .entry(field_name.clone())
                            .or_default()
                            .insert(type_name.clone());
                    }
                    fields_to_rebaseable_types
                        .entry(INTROSPECTION_TYPENAME_FIELD_NAME.clone())
                        .or_default()
                        .insert(type_name.clone());
                    // Record the object type as implementing itself.
                    let implementing_composite_types = object_types_to_implementing_composite_types
                        .entry(type_name.clone())
                        .or_default();
                    implementing_composite_types.insert(type_name.clone());
                    // For each implements, record the interface type as an implementing type.
                    if !type_.implements_interfaces.is_empty() {
                        implementing_composite_types.extend(
                            type_
                                .implements_interfaces
                                .iter()
                                .map(|interface_name| interface_name.name.clone()),
                        );
                    }
                }
                ExtendedType::Interface(type_) => {
                    // Record fields that don't contain @fromContext as being rebaseable (also
                    // including __typename).
                    for (field_name, field_definition) in &type_.fields {
                        if field_definition.arguments.iter().any(|arg_definition| {
                            arg_definition
                                .directives
                                .has(from_context_directive_definition_name)
                        }) {
                            continue;
                        }
                        fields_to_rebaseable_types
                            .entry(field_name.clone())
                            .or_default()
                            .insert(type_name.clone());
                    }
                    fields_to_rebaseable_types
                        .entry(INTROSPECTION_TYPENAME_FIELD_NAME.clone())
                        .or_default()
                        .insert(type_name.clone());
                }
                ExtendedType::Union(type_) => {
                    // Just record the __typename field as being rebaseable.
                    fields_to_rebaseable_types
                        .entry(INTROSPECTION_TYPENAME_FIELD_NAME.clone())
                        .or_default()
                        .insert(type_name.clone());
                    // For each member, record the union type as an implementing type.
                    for member_name in &type_.members {
                        object_types_to_implementing_composite_types
                            .entry(member_name.name.clone())
                            .or_default()
                            .insert(type_name.clone());
                    }
                }
                _ => {}
            }
        }

        // With the interface implements/union membership relationships, we can compute which
        // pairs of types have at least one possible runtime type in their intersection, and
        // are thus rebaseable.
        let mut inline_fragments_to_rebaseable_types: IndexMap<Name, IndexSet<Name>> =
            Default::default();
        for implementing_types in object_types_to_implementing_composite_types.values() {
            for type_name in implementing_types {
                inline_fragments_to_rebaseable_types
                    .entry(type_name.clone())
                    .or_default()
                    .extend(implementing_types.iter().cloned())
            }
        }

        // Finally, we can compute the nodes for the rebaseable types, as we'll be working with
        // those instead of types when checking whether an operation element can be rebased.
        let types_to_nodes = graph.types_to_nodes_by_source(subgraph_name)?;
        for (field_name, types) in fields_to_rebaseable_types {
            metadata
                .fields_to_rebaseable_parent_nodes
                .entry(field_name)
                .or_default()
                .extend(
                    types
                        .iter()
                        .flat_map(|type_| types_to_nodes.get(type_).map(|nodes| nodes.iter()))
                        .flatten()
                        .cloned(),
                );
        }
        for (type_condition_name, types) in inline_fragments_to_rebaseable_types {
            metadata
                .inline_fragments_to_rebaseable_parent_nodes
                .entry(type_condition_name)
                .or_default()
                .extend(
                    types
                        .iter()
                        .flat_map(|type_| types_to_nodes.get(type_).map(|nodes| nodes.iter()))
                        .flatten()
                        .cloned(),
                )
        }
    }
    Ok(metadata)
}

/// During query graph creation, we pre-compute metadata that helps us greatly speed up the
/// estimation process during request execution. The expected time and memory consumed for
/// pre-computation is (in the worst case) expected to be on the order of the number of nodes
/// plus the number of edges.
///
/// Note that when the below field docs talk about a "complete digraph", they are referring to
/// the graph theory concept: https://en.wikipedia.org/wiki/Complete_graph
#[derive(Debug, Default)]
pub(crate) struct QueryGraphMetadata {
    /// When a (resolvable) @key exists on a type T in a subgraph, a key resolution edge is
    /// created from every subgraph's type T to that subgraph's type T. This similarly holds for
    /// root type resolution edges. This means that the nodes of type T with such a @key (or are
    /// operation root types) form a complete digraph in the query graph. These indirect options
    /// effectively occur as a group in our estimation process, so we track group members here
    /// per type name, and precompute units of work relative to these groups.
    ///
    /// Interface object types I in a subgraph will only sometimes create a key resolution edge
    /// between an implementing type T in a subgraph and that subgraph's type I. This means the
    /// nodes of the complete digraph for I are indirect options for such nodes of type T. We
    /// track any such types I that are reachable for at least one node in the complete digraph
    /// for type T here as well.
    types_to_indirect_options: IndexMap<Name, IndirectOptionsMetadata>,
    /// For nodes of a type T that aren't in their complete digraph (due to not having a @key),
    /// these remaining nodes will have the complete digraph of T (and any interface object
    /// complete digraphs) as indirect options, but these remaining nodes may separately have
    /// more indirect options that are not options for the complete digraph of T, specifically
    /// if the complete digraph for T has no key resolution edges to an interface object I, but
    /// this remaining node does. We keep track of such interface object types for those
    /// remaining nodes here.
    remaining_nodes_to_interface_object_options: IndexMap<NodeIndex, IndexSet<Name>>,
    /// A map of field names to the endpoints of field query graph edges with that field name. Note
    /// we additionally store the progressive overrides label, if the edge is conditioned on it.
    fields_to_endpoints: IndexMap<Name, IndexMap<NodeIndex, FieldTarget>>,
    /// A map of type condition names to endpoints of downcast query graph edges with that type
    /// condition name, including fake downcasts for interface objects, and a non-existent edge that
    /// represents a type condition name equal to the parent type.
    inline_fragments_to_endpoints: IndexMap<Name, IndexMap<NodeIndex, NodeIndex>>,
    /// A map of composite type nodes to their downcast edges that lead specifically to an object
    /// type (i.e., the possible runtime types of the node's type).
    nodes_to_object_type_downcasts: IndexMap<NodeIndex, ObjectTypeDowncasts>,
    /// A map of field names to parent nodes whose corresponding type and schema can be rebased on
    /// by the field.
    fields_to_rebaseable_parent_nodes: IndexMap<Name, IndexSet<NodeIndex>>,
    /// A map of type condition names to parent nodes whose corresponding type and schema can be
    /// rebased on by an inline fragment with that type condition.
    inline_fragments_to_rebaseable_parent_nodes: IndexMap<Name, IndexSet<NodeIndex>>,
}

/// Indirect option metadata for the complete digraph for type T. See [QueryGraphMetadata] for
/// more information about how we group indirect options into complete digraphs.
#[derive(Debug, Default)]
pub(crate) struct IndirectOptionsMetadata {
    /// The members of the complete digraph for type T.
    same_type_options: IndexSet<NodeIndex>,
    /// Any interface object types I that are reachable for at least one node in the complete
    /// digraph for type T.
    interface_object_options: IndexSet<Name>,
}

#[derive(Debug)]
enum FieldTarget {
    /// Normal non-overridden fields, which don't have a label condition.
    NonOverride(NodeIndex),
    /// Overridden fields, which have a label condition.
    Override(NodeIndex, OverrideCondition),
}

#[derive(Debug)]
enum ObjectTypeDowncasts {
    /// Normal non-interface-object types have regular downcasts to their object type nodes.
    NonInterfaceObject(IndexMap<Name, NodeIndex>),
    /// Interface object types have "fake" downcasts to nodes that are really the self node.
    InterfaceObject(IndexSet<Name>),
}

#[derive(Debug, Default)]
pub(crate) struct State {
    /// An estimation of the number of non-local selections for the whole operation (where the count
    /// for a given selection set is scaled by the number of tail nodes at that selection set). Note
    /// this does not count selections from recursive query planning.
    pub(crate) count: u64,
    /// Whenever we take a selection on a set of nodes with indirect options, we cache the
    /// resulting nodes here.
    next_nodes_cache: IndexMap<SelectionKey, NextNodesCache>,
}

#[derive(Debug, PartialEq, Eq, Hash)]
enum SelectionKey {
    /// For field selections, this is the field's name.
    Field(Name),
    /// For inline fragment selections, this is the type condition's name.
    InlineFragment(Name),
}

/// See [QueryGraphMetadata] for more information about how we group indirect options into
/// complete digraphs.
#[derive(Debug, Default)]
struct NextNodesCache {
    /// This is the merged next node info for selections on the set of nodes in the complete
    /// digraph for the given type T. Note that this does not merge in the next node info for
    /// any interface object options reachable from nodes in that complete digraph for T.
    types_to_next_nodes: IndexMap<Name, NextNodesInfo>,
    /// This is the next node info for selections on the given node. Note that this does not
    /// merge in the next node info for any interface object options reachable from that node.
    remaining_nodes_to_next_nodes: IndexMap<NodeIndex, NextNodesInfo>,
}

#[derive(Clone, Debug, Default)]
struct NextNodesInfo {
    /// The next nodes after taking the selection.
    next_nodes: IndexSet<NodeIndex>,
    /// Whether any cross-subgraph edges are reachable from any next nodes.
    next_nodes_have_reachable_cross_subgraph_edges: bool,
    /// These are the next nodes along with indirect options, represented succinctly by the
    /// types of any complete digraphs along with remaining nodes.
    next_nodes_with_indirect_options: NodesWithIndirectOptionsInfo,
}

impl NextNodesInfo {
    fn extend(&mut self, other: &Self) {
        self.next_nodes.extend(other.next_nodes.iter().cloned());
        self.next_nodes_have_reachable_cross_subgraph_edges = self
            .next_nodes_have_reachable_cross_subgraph_edges
            || other.next_nodes_have_reachable_cross_subgraph_edges;
        self.next_nodes_with_indirect_options
            .extend(&other.next_nodes_with_indirect_options);
    }
}

#[derive(Clone, Debug, Default)]
struct NodesWithIndirectOptionsInfo {
    /// For indirect options that are representable as complete digraphs for a type T, these are
    /// those types.
    types: IndexSet<Name>,
    /// For any nodes of type T that aren't in their complete digraphs for type T, these are
    /// those nodes.
    remaining_nodes: IndexSet<NodeIndex>,
}

impl NodesWithIndirectOptionsInfo {
    fn extend(&mut self, other: &Self) {
        self.types.extend(other.types.iter().cloned());
        self.remaining_nodes
            .extend(other.remaining_nodes.iter().cloned());
    }
}
