//! Applying a routing choice to the plan state: creating entity fetch
//! groups, wiring dependency edges and entity inputs, and dispatching
//! sub-selections back onto the pending stack.

use std::sync::Arc;

use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;
use tracing::trace;

use super::super::defer;
use super::super::defer::strip_defer_directive;
use super::super::fetch_graph::InputContribution;
use super::super::fetch_graph::InputRewriteInfo;
use super::super::shared_path::SharedPath;
use super::FieldRoutingSearchSpace;
use crate::error::FederationError;
use crate::operation::Field;
use crate::operation::FieldSelection;
use crate::operation::HasSelectionKey;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::query_graph::ContextCondition;
use crate::query_graph::QueryGraphEdgeTransition;
use crate::query_graph::QueryGraphNodeType;
use crate::query_graph::graph_path::operation::OpPathElement;
use crate::query_plan::FetchDataPathElement;
use crate::schema::position::CompositeTypeDefinitionPosition;

const CONDITION_DEPTH_LIMIT: u8 = 32;
use super::NodeSource;
use super::context;
use super::requires::trailing_condition_fragments;
use super::requires::unconditioned_input_path;
use super::routing::HopKind;
use super::routing::RoutingChoice;
use super::selection_label;
use super::state::ContextAnchor;
use super::state::PendingSelection;
use super::state::PlanState;

/// Shared inputs of one `commit_choice` invocation, threaded through the
/// @requires stage.
pub(super) struct CommitCtx<'a> {
    pub(super) pending: &'a PendingSelection,
    pub(super) choice: &'a RoutingChoice,
    /// The parent-to-entity dependency edge, when the choice was a hop.
    /// @requires inputs ride on this edge.
    pub(super) key_hop_edge: Option<EdgeIndex>,
}

/// Where a committed selection's children begin: fetch node, operation
/// path, and response path.
pub(super) struct CommitTarget {
    pub(super) fetch_node: NodeIndex,
    pub(super) op_path: SharedPath<Arc<OpPathElement>>,
    pub(super) response_path: SharedPath<FetchDataPathElement>,
    /// True at the root of an entity fetch group (key hop or same-subgraph
    /// @requires hop): children restart their type path there.
    #[allow(dead_code)]
    pub(super) entity_root: bool,
}

impl FieldRoutingSearchSpace {
    /// Commit a routing choice for `pending`: wire key hops, place the
    /// selection in the appropriate fetch group, and push child pendings
    /// for sub-selections.
    pub(super) fn commit_choice(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
        choice: &RoutingChoice,
    ) -> Result<(), FederationError> {
        use super::routing::RoutingTarget;

        if matches!(choice.target, RoutingTarget::TypeExplosion) {
            return if self.try_explode_interface_field(state, pending)? {
                Ok(())
            } else {
                Err(FederationError::internal(
                    "type explosion chosen but inapplicable at this position",
                ))
            };
        }

        if matches!(choice.target, RoutingTarget::RestructureFragment) {
            return if self.try_pass_through_fragment(state, pending)?
                || self.try_vacuous_type_condition(state, pending)?
                || self.try_explode_abstract_type(state, pending)?
            {
                Ok(())
            } else {
                Err(FederationError::internal(
                    "fragment restructure chosen but inapplicable at this position",
                ))
            };
        }

        let qg = self.qg();
        let (_, target_qg_node) = qg.edge_endpoints(choice.edge_index())?;

        // Reject unexpected edge transitions before any mutation, so a
        // failed commit leaves nothing behind and the caller counts the
        // drop.
        let Some(response_path_elements) =
            self.response_path_for_edge(choice.edge_index(), &pending.selection)?
        else {
            return Err(FederationError::internal(format!(
                "unexpected edge transition committing {}",
                selection_label(&pending.selection),
            )));
        };

        // Mutating half: commit the hop or resolve the direct fetch group.
        let (fetch_node, key_hop_edge) = match choice.hop_kind {
            HopKind::RootHop => {
                let (group, hop_edge) = self.commit_root_hop(state, pending, choice)?;
                (group, Some(hop_edge))
            }
            HopKind::KeyHop => {
                let (group, hop_edge) = self.commit_key_hop(state, pending, choice)?;
                (group, Some(hop_edge))
            }
            HopKind::Direct => (self.direct_fetch_node(state, pending, choice)?, None),
        };

        // Pure half: assemble op and response paths for children.
        let mut target = self.target_paths(
            pending,
            choice,
            choice.hop_kind,
            fetch_node,
            response_path_elements,
        )?;

        let ctx = CommitCtx {
            pending,
            choice,
            key_hop_edge,
        };
        let edge = qg.edge_weight(choice.edge_index())?;
        if let Some(requires_conditions) = &edge.conditions {
            target = self.apply_requires(state, &ctx, requires_conditions, target)?;
        }
        if !edge.required_contexts.is_empty() {
            target = self.apply_contexts(state, &ctx, &edge.required_contexts, target)?;
        }

        // Condition selections carry an ordering dependent: their consuming
        // group must run after every group they commit into. A would-be
        // cycle fails the commit; BULB treats that as a dead branch and
        // backtracks.
        if let Some(dependent) = pending.ordering_dependent() {
            state
                .graph
                .add_ordering_dependency(target.fetch_node, dependent)?;
        }

        self.dispatch_sub_selections(state, pending, target_qg_node, &target)
    }

    /// Commit a root-type-resolution hop: creates a root-hop group in the
    /// target subgraph instead of an entity group (no _entities query,
    /// executes a fresh root operation).
    fn commit_root_hop(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
        choice: &RoutingChoice,
    ) -> Result<(NodeIndex, EdgeIndex), FederationError> {
        let qg = self.qg();
        trace!(
            target_subgraph = %choice.target_subgraph(),
            "committing root type resolution hop",
        );
        let source = self.node_source(pending.query_graph_node)?;
        self.append_typename(state, pending.fetch_node, &pending.op_path, &source);

        let merge_at = self.pending_merge_at(state, pending);

        let (field_source, _) = qg.edge_endpoints(choice.edge_index())?;
        let field_source_node = qg.node_weight(field_source)?;
        let root_type: CompositeTypeDefinitionPosition =
            field_source_node.type_.clone().try_into()?;

        let new_group =
            state
                .graph
                .add_root_hop_group(choice.target_subgraph(), root_type, merge_at);

        let edge = state
            .graph
            .add_dependency(pending.fetch_node, new_group, Vec::new());

        Ok((new_group, edge))
    }

    /// Commit a key-resolution hop: creates an entity group in the target
    /// subgraph, wires key inputs and dependency edges.
    pub(super) fn commit_key_hop(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
        choice: &RoutingChoice,
    ) -> Result<(NodeIndex, EdgeIndex), FederationError> {
        let qg = self.qg();
        trace!(
            target_subgraph = %choice.target_subgraph(),
            selection = %selection_label(&pending.selection),
            merge_at = ?self.pending_merge_at(state, pending),
            "committing key hop",
        );
        let source = self.node_source(pending.query_graph_node)?;

        // Key fields must appear at the parent's merge path to build the
        // entity representation. `conditions_provided` short-circuits the
        // in-place re-derivation: enumeration already verified, against the
        // query graph at this exact position, that an ancestor's @provides
        // makes every key field available here (the subgraph echoes provided
        // fields), including through downcasts out of the provides-copy
        // layer that `can_resolve_in_place` cannot see.
        let key_locally_resolvable = match &choice.key_conditions {
            Some(key_conditions) => {
                choice.conditions_provided
                    || self.can_resolve_in_place(
                        pending.query_graph_node,
                        key_conditions,
                        &source,
                    )?
            }
            None => true,
        };

        // Key fields plus __typename, which identifies the entity type.
        let anchor_key = key_locally_resolvable
            .then_some(choice.key_conditions.as_ref())
            .flatten();
        self.append_entity_inputs(
            state,
            pending.fetch_node,
            &pending.op_path,
            anchor_key,
            &source,
        );

        let merge_at = self.pending_merge_at(state, pending);

        // The group the key edge enters: for a chained hop, the first
        // intermediate, not the final target.
        let (first_subgraph, first_dest_node) = match choice.intermediate_key_hops.first() {
            Some(hop) => (&hop.target_subgraph, Some(hop.target_node)),
            None => (choice.target_subgraph(), None),
        };

        let new_group = self.entity_group_avoiding_cycles(
            state,
            first_subgraph,
            merge_at.clone(),
            pending.defer_ref.clone(),
            pending.fetch_node,
            pending.ordering_dependent(),
        );

        let key_input = if let Some(key_conditions) = &choice.key_conditions {
            let dest_node = match first_dest_node {
                Some(node) => node,
                None => qg.edge_endpoints(choice.edge_index())?.0,
            };
            let dest_type: CompositeTypeDefinitionPosition =
                qg.node_weight(dest_node)?.type_.clone().try_into()?;

            Some(InputContribution {
                source_type_name: source.type_pos.type_name().clone(),
                conditions: key_conditions.clone(),
                rewrite_info: Some(InputRewriteInfo {
                    dest_type,
                    dest_subgraph: first_subgraph.clone(),
                }),
                condition_alias_rewrites: Vec::new(),
            })
        } else {
            None
        };

        let edge = self.wire_key_edge(state, pending.fetch_node, new_group, key_input, true);

        // Keys the current fetch cannot resolve directly are routed as
        // pending selections; ordering edges to the new group are wired as
        // they commit. Statically circular keys are the exception: routing
        // their conditions would recurse without progress, so they are
        // handled via locally_satisfiable_subset instead.
        if !key_locally_resolvable && let Some(key_conditions) = &choice.key_conditions {
            if choice.conditions_unroutable {
                self.commit_circular_key_conditions(
                    state,
                    pending,
                    key_conditions,
                    &source,
                    pending.fetch_node,
                    &pending.op_path,
                    new_group,
                )?;
            } else {
                self.push_condition_pendings(state, pending, key_conditions, new_group)?;
            }
        }

        // Multi-hop key chain: walk through intermediate subgraphs,
        // creating an entity group for each hop that feeds the next.
        if !choice.intermediate_key_hops.is_empty() {
            return self.commit_intermediate_hops(state, pending, choice, new_group, merge_at);
        }

        Ok((new_group, edge))
    }

    /// Handle a circular key at commit time: its conditions can't be
    /// independently routed: pushing them as pendings would recurse without
    /// progress. If the anchor can resolve the whole key, select it there; a
    /// key it can only partially resolve can never match an entity at
    /// runtime, so fail the commit and let backtracking look for an
    /// alternative instead of emitting a fetch that is dead on arrival.
    #[allow(clippy::too_many_arguments)]
    fn commit_circular_key_conditions(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
        key_conditions: &Arc<SelectionSet>,
        source: &NodeSource,
        anchor_fetch: NodeIndex,
        anchor_path: &SharedPath<Arc<OpPathElement>>,
        new_group: NodeIndex,
    ) -> Result<(), FederationError> {
        let resolvable =
            self.locally_satisfiable_subset(key_conditions, &source.type_pos, &source.schema);
        let covers_key = resolvable.as_ref().is_some_and(|subset| {
            key_conditions
                .selections
                .values()
                .all(|sel| subset.selections.contains_key(sel.key()))
        });
        if !covers_key {
            return Err(FederationError::internal(format!(
                "circular key conditions unsatisfiable at {}: {}",
                source.type_pos.type_name(),
                key_conditions,
            )));
        }
        if let Some(resolvable) = &resolvable {
            self.append_entity_inputs(state, anchor_fetch, anchor_path, Some(resolvable), source);
            self.push_condition_pendings(state, pending, resolvable, new_group)?;
        }
        Ok(())
    }

    fn commit_intermediate_hops(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
        choice: &RoutingChoice,
        first_group: NodeIndex,
        merge_at: Vec<FetchDataPathElement>,
    ) -> Result<(NodeIndex, EdgeIndex), FederationError> {
        let qg = self.qg();
        let mut prev_group = first_group;

        for (i, hop) in choice.intermediate_key_hops.iter().enumerate() {
            let hop_node_data = qg.node_weight(hop.target_node)?;
            let hop_type_pos: CompositeTypeDefinitionPosition =
                hop_node_data.type_.clone().try_into()?;

            let is_last = i + 1 == choice.intermediate_key_hops.len();
            let next_subgraph = if is_last {
                choice.target_subgraph()
            } else {
                &choice.intermediate_key_hops[i + 1].target_subgraph
            };

            let next_group = self.entity_group_avoiding_cycles(
                state,
                next_subgraph,
                merge_at.clone(),
                pending.defer_ref.clone(),
                prev_group,
                pending.ordering_dependent(),
            );

            if let Some(key_conds) = &hop.key_conditions {
                let hop_schema = qg.schema_by_source(&hop_node_data.source)?.clone();
                let hop_source = NodeSource {
                    type_pos: hop_type_pos.clone(),
                    subgraph: hop.target_subgraph.clone(),
                    schema: hop_schema,
                };
                let hop_path = self.entity_root_path(hop_type_pos.type_name())?;
                if self.can_resolve_in_place(hop.target_node, key_conds, &hop_source)? {
                    self.append_entity_inputs(
                        state,
                        prev_group,
                        &hop_path,
                        Some(key_conds),
                        &hop_source,
                    );
                } else {
                    self.append_entity_inputs(state, prev_group, &hop_path, None, &hop_source);
                    let hop_anchor = pending
                        .fork(pending.selection.clone())
                        .at(hop.target_node, prev_group)
                        .with_op_path(hop_path.clone())
                        .with_response_path(SharedPath::new())
                        .with_provides_anchor(None);
                    self.push_condition_pendings(state, &hop_anchor, key_conds, next_group)?;
                }
            }

            let hop_key_input = if let Some(key_conds) = &hop.key_conditions {
                let dest_type: CompositeTypeDefinitionPosition = if is_last {
                    let (field_source, _) = qg.edge_endpoints(choice.edge_index())?;
                    qg.node_weight(field_source)?.type_.clone().try_into()?
                } else {
                    qg.node_weight(choice.intermediate_key_hops[i + 1].target_node)?
                        .type_
                        .clone()
                        .try_into()?
                };
                Some(InputContribution {
                    source_type_name: hop_type_pos.type_name().clone(),
                    conditions: key_conds.clone(),
                    rewrite_info: Some(InputRewriteInfo {
                        dest_type,
                        dest_subgraph: next_subgraph.clone(),
                    }),
                    condition_alias_rewrites: Vec::new(),
                })
            } else {
                None
            };

            let hop_edge = self.wire_key_edge(state, prev_group, next_group, hop_key_input, true);

            if is_last {
                return Ok((next_group, hop_edge));
            }
            prev_group = next_group;
        }

        unreachable!("intermediate_key_hops is non-empty")
    }

    /// Get or create the entity group for (subgraph, merge_at), falling
    /// back to a fresh group when reuse would create a dependency cycle.
    fn entity_group_avoiding_cycles(
        &self,
        state: &mut PlanState,
        subgraph: &Arc<str>,
        merge_at: Vec<FetchDataPathElement>,
        defer_ref: Option<String>,
        anchor_fetch: NodeIndex,
        ordering_dependent: Option<NodeIndex>,
    ) -> NodeIndex {
        let group = state.graph.get_or_create_entity_group_with_defer(
            subgraph,
            merge_at.clone(),
            defer_ref.clone(),
        );
        let conflicts_with_dependent = ordering_dependent
            .is_some_and(|dep| group == dep || state.graph.is_reachable(dep, group));
        if conflicts_with_dependent || state.graph.is_reachable(group, anchor_fetch) {
            return state
                .graph
                .add_entity_group_with_defer(subgraph, merge_at, defer_ref);
        }
        group
    }

    /// Find or create the anchor->group dependency edge and attach the key
    /// input. With `dedupe_same_type_key`, an edge already carrying a key
    /// for the input's source type is left alone.
    fn wire_key_edge(
        &self,
        state: &mut PlanState,
        anchor: NodeIndex,
        group: NodeIndex,
        key_input: Option<InputContribution>,
        dedupe_same_type_key: bool,
    ) -> EdgeIndex {
        if let Some(existing_edge) = state.graph.find_edge(anchor, group) {
            if let Some(input) = key_input
                && !(dedupe_same_type_key
                    && state
                        .graph
                        .edge_has_key_input(existing_edge, &input.source_type_name))
            {
                state.graph.add_input_to_edge(existing_edge, input);
            }
            existing_edge
        } else {
            state
                .graph
                .add_dependency(anchor, group, key_input.into_iter().collect())
        }
    }

    /// The fetch group a direct (non-hop) choice lands in: fields may create
    /// root groups on demand ([`Self::field_fetch_node`]); inline fragments
    /// stay in the current group. An @interfaceObject fake downcast
    /// additionally pushes a best-effort concrete-`__typename` pending:
    /// execution needs each object's concrete typename to test the condition,
    /// which the io subgraph cannot supply
    /// ([`Self::push_interface_object_typename`]).
    fn direct_fetch_node(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
        choice: &RoutingChoice,
    ) -> Result<NodeIndex, FederationError> {
        match &pending.selection {
            Selection::Field(_) => self.field_fetch_node(state, pending, choice),
            Selection::InlineFragment(_) => {
                let edge = self
                    .cached_query_graph
                    .query_graph
                    .edge_weight(choice.edge_index())?;
                if matches!(
                    edge.transition,
                    QueryGraphEdgeTransition::InterfaceObjectFakeDownCast { .. }
                ) {
                    self.push_interface_object_typename(state, pending)?;
                }
                Ok(pending.fetch_node)
            }
        }
    }

    /// @interfaceObject subgraph can only report the interface's typename:
    /// execution needs each object's concrete `__typename` to test the
    /// condition. Push a `__typename` pending at the current position and
    /// let the generic routing machinery satisfy it. The "not in this
    /// subgraph" constraint is already encoded in the query graph:
    /// @interfaceObject types get no `__typename` FieldCollection edge (see
    /// `add_object_type_edges`), so the pending has no direct option here
    /// and routes only via key hops to subgraphs owning the real interface.
    fn push_interface_object_typename(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
    ) -> Result<(), FederationError> {
        let source = self.node_source(pending.query_graph_node)?;
        let supergraph_pos: CompositeTypeDefinitionPosition = self
            .supergraph_schema
            .get_type(source.type_pos.type_name())?
            .try_into()?;
        let typename = Selection::Field(Arc::new(FieldSelection {
            field: Field::new_introspection_typename(
                &self.supergraph_schema,
                &supergraph_pos,
                None,
            ),
            selection_set: None,
        }));
        state.push_pending(pending.fork(typename).into_best_effort());
        Ok(())
    }

    /// The fetch group a directly-routed field lands in: the pending's own
    /// fetch node, except at the FederatedRootType head (root group created
    /// on demand; the pending entry holds a placeholder).
    fn field_fetch_node(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
        choice: &RoutingChoice,
    ) -> Result<NodeIndex, FederationError> {
        let qg = self.qg();
        let current_node_data = qg.node_weight(pending.query_graph_node)?;
        if matches!(
            current_node_data.type_,
            QueryGraphNodeType::FederatedRootType(_)
        ) {
            let (field_source, _) = qg.edge_endpoints(choice.edge_index())?;
            let subgraph_node = qg.node_weight(field_source)?;
            let root_type: CompositeTypeDefinitionPosition =
                subgraph_node.type_.clone().try_into()?;
            return Ok(state.graph.get_or_create_root_group_with_defer(
                &subgraph_node.source,
                root_type,
                pending.defer_ref.clone(),
            ));
        }

        Ok(pending.fetch_node)
    }

    /// The response-path elements a routed edge contributes for result
    /// merging: field collections contribute their response key plus one
    /// `@` per list nesting level; downcasts contribute nothing. Returns
    /// `None` for transition kinds that should never be routed here.
    pub(super) fn response_path_for_edge(
        &self,
        edge_index: EdgeIndex,
        selection: &Selection,
    ) -> Result<Option<Vec<FetchDataPathElement>>, FederationError> {
        let qg = self.qg();
        let edge = qg.edge_weight(edge_index)?;
        match &edge.transition {
            QueryGraphEdgeTransition::FieldCollection {
                source,
                field_definition_position,
                ..
            } => {
                let response_key = match selection {
                    Selection::Field(f) => f.field.response_name().clone(),
                    _ => field_definition_position.field_name().clone(),
                };
                let mut elements =
                    vec![FetchDataPathElement::Key(response_key, Default::default())];
                let field_schema = qg.schema_by_source(source)?;
                let mut type_ = &field_definition_position.get(field_schema.schema())?.ty;
                loop {
                    match type_ {
                        apollo_compiler::ast::Type::Named(_)
                        | apollo_compiler::ast::Type::NonNullNamed(_) => break,
                        apollo_compiler::ast::Type::List(inner)
                        | apollo_compiler::ast::Type::NonNullList(inner) => {
                            elements.push(FetchDataPathElement::AnyIndex(Default::default()));
                            type_ = inner;
                        }
                    }
                }
                Ok(Some(elements))
            }
            QueryGraphEdgeTransition::Downcast { .. }
            | QueryGraphEdgeTransition::InterfaceObjectFakeDownCast { .. } => Ok(Some(vec![])),
            _ => Ok(None),
        }
    }

    /// Pure path assembly for a committed choice: where children's op and
    /// response paths start. Mutates nothing; all group/edge creation
    /// happens earlier in `commit_choice`.
    fn target_paths(
        &self,
        pending: &PendingSelection,
        choice: &RoutingChoice,
        hop: HopKind,
        fetch_node: NodeIndex,
        response_path_elements: Vec<FetchDataPathElement>,
    ) -> Result<CommitTarget, FederationError> {
        let qg = self.qg();
        let op_path = match hop {
            // Hops restart the op path at the new group's root: empty for
            // root hops; for key hops, `... on <ConcreteType>` (entity
            // fetches start from the _Entity union) plus any trailing
            // @skip/@include fragments.
            HopKind::RootHop | HopKind::KeyHop => {
                let base = if hop == HopKind::RootHop {
                    SharedPath::new()
                } else {
                    let (field_source, _) = qg.edge_endpoints(choice.edge_index())?;
                    let dest = self.node_source(field_source)?;
                    let mut initial_path = self.entity_root_path(dest.type_pos.type_name())?;
                    for element in trailing_condition_fragments(&pending.op_path) {
                        initial_path = initial_path.pushed(element);
                    }
                    initial_path
                };
                let op_element: Arc<OpPathElement> = match &pending.selection {
                    Selection::Field(field_sel) => {
                        Arc::new(OpPathElement::Field(field_sel.field.clone()))
                    }
                    Selection::InlineFragment(frag_sel) => Arc::new(OpPathElement::InlineFragment(
                        strip_defer_directive(&frag_sel.inline_fragment),
                    )),
                };
                base.pushed(op_element)
            }
            // Direct choices extend the current op path with this selection.
            HopKind::Direct => match &pending.selection {
                Selection::Field(field_sel) => pending
                    .op_path
                    .pushed(Arc::new(OpPathElement::Field(field_sel.field.clone()))),
                Selection::InlineFragment(frag_sel) => {
                    let stripped = strip_defer_directive(&frag_sel.inline_fragment);
                    let edge = qg.edge_weight(choice.edge_index())?;
                    if matches!(
                        edge.transition,
                        QueryGraphEdgeTransition::InterfaceObjectFakeDownCast { .. }
                    ) {
                        // @interfaceObject fake downcast: the concrete type
                        // doesn't exist in this subgraph.
                        if stripped.directives.is_empty() {
                            pending.op_path.clone()
                        } else {
                            let updated = stripped.with_updated_type_condition(None);
                            pending
                                .op_path
                                .pushed(Arc::new(OpPathElement::InlineFragment(updated)))
                        }
                    } else {
                        pending
                            .op_path
                            .pushed(Arc::new(OpPathElement::InlineFragment(stripped)))
                    }
                }
            },
        };

        // Hops restart the response path at the new fetch node's root.
        // Only direct choices continue from the pending's current position.
        let response_path = {
            let mut rp = if hop == HopKind::Direct {
                pending.path_in_fetch.clone()
            } else {
                SharedPath::new()
            };
            for element in response_path_elements {
                rp = rp.pushed(element);
            }
            rp
        };

        Ok(CommitTarget {
            fetch_node,
            op_path,
            response_path,
            entity_root: choice.hop_kind != HopKind::Direct,
        })
    }

    /// Merge path for a new group created for `pending`: the parent group's
    /// merge_at plus the pending's path within it.
    pub(super) fn pending_merge_at(
        &self,
        state: &PlanState,
        pending: &PendingSelection,
    ) -> Vec<FetchDataPathElement> {
        let mut merge_at = state.graph.merge_at(pending.fetch_node).to_vec();
        merge_at.extend(pending.path_in_fetch.iter().cloned());
        merge_at
    }

    /// Select entity-representation inputs (key fields when given, plus
    /// __typename) in `fetch_node` at the unconditioned input path.
    pub(super) fn append_entity_inputs(
        &self,
        state: &mut PlanState,
        fetch_node: NodeIndex,
        op_path: &SharedPath<Arc<OpPathElement>>,
        key_conditions: Option<&Arc<SelectionSet>>,
        source: &NodeSource,
    ) {
        let input_path = unconditioned_input_path(op_path);
        if let Some(key) = key_conditions {
            state
                .graph
                .append_selection(fetch_node, &input_path, Some(key));
        }
        self.append_typename(state, fetch_node, &input_path, source);
    }

    /// Handle @fromContext on the routed edge: resolve context values from
    /// ancestors and pass them via entity inputs, possibly redirecting the
    /// field into a context-isolating entity fetch.
    pub(super) fn apply_contexts(
        &self,
        state: &mut PlanState,
        ctx: &CommitCtx<'_>,
        required_contexts: &[ContextCondition],
        target: CommitTarget,
    ) -> Result<CommitTarget, FederationError> {
        let (fetch_node, op_path) = context::handle_from_context(
            self,
            state,
            ctx,
            target.fetch_node,
            &target.op_path,
            required_contexts,
        )?;
        Ok(CommitTarget {
            fetch_node,
            op_path,
            ..target
        })
    }

    pub(super) fn push_condition_pendings(
        &self,
        state: &mut PlanState,
        anchor: &PendingSelection,
        conditions_arc: &Arc<SelectionSet>,
        dependent: NodeIndex,
    ) -> Result<(), FederationError> {
        let conditions: &SelectionSet = conditions_arc;
        // Bound requires-of-requires nesting: mutually recursive @requires
        // would otherwise alternate forever, minting fresh entity groups
        // each round.
        if anchor.condition_depth() >= CONDITION_DEPTH_LIMIT {
            return Err(FederationError::internal(format!(
                "condition resolution nested more than {CONDITION_DEPTH_LIMIT} levels deep (circular @requires?)",
            )));
        }
        // Condition data is entity-fetch input: route it unconditionally
        // (see `unconditioned_input_path`).
        let input_path = unconditioned_input_path(&anchor.op_path);
        for sel in conditions.selections.values().rev().cloned() {
            let mut forked = anchor.fork(sel).into_condition_for(dependent);
            forked.op_path = input_path.clone();
            state.push_pending(forked);
        }
        Ok(())
    }

    /// Always emit `__typename` under abstract-typed fields so the fetch
    /// has at least `{ field { __typename } }` even if every child fragment
    /// is later dropped by the doom penalty.
    fn ensure_abstract_typename(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
        target_qg_node: NodeIndex,
        fetch_node: NodeIndex,
        op_path: &SharedPath<Arc<OpPathElement>>,
    ) -> Result<(), FederationError> {
        if let Selection::Field(_) = &pending.selection {
            let target_data = self
                .cached_query_graph
                .query_graph
                .node_weight(target_qg_node)?;
            if let Ok(target_pos) =
                CompositeTypeDefinitionPosition::try_from(target_data.type_.clone())
                && target_pos.is_abstract_type()
            {
                let target_source = self.node_source(target_qg_node)?;
                self.append_typename(state, fetch_node, op_path, &target_source);
            }
        }
        Ok(())
    }

    /// Final phase of `commit_choice`: record a leaf selection in the fetch
    /// node or push sub-selections onto the pending stack for individual
    /// routing.
    pub(super) fn dispatch_sub_selections(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
        target_qg_node: NodeIndex,
        target: &CommitTarget,
    ) -> Result<(), FederationError> {
        let fetch_node = target.fetch_node;

        self.ensure_abstract_typename(state, pending, target_qg_node, fetch_node, &target.op_path)?;

        let Some(sub_ss) = pending
            .selection
            .selection_set()
            .filter(|ss| !ss.is_empty())
        else {
            // Leaf selection: record in the selection builder at the full
            // path.
            state
                .graph
                .append_selection(fetch_node, &target.op_path, None);
            return Ok(());
        };

        let child_provides_anchor =
            self.child_provides_anchor(pending, target_qg_node, target.entity_root)?;

        // When the committed selection is an inline fragment carrying
        // @defer, extract the label and propagate it to children so fetch
        // nodes created downstream land in the deferred partition.
        let child_defer_ref = defer::defer_context(&pending.selection)
            .0
            .or_else(|| pending.defer_ref.clone());

        // Extend parent_types with the current target type for @fromContext
        // ancestor resolution.
        let target_node_data = self
            .cached_query_graph
            .query_graph
            .node_weight(target_qg_node)?;
        let child_parent_types = {
            let mut types = pending.parent_types.clone();
            // From a FederatedRootType node the root type (e.g. Query) is not
            // in parent_types yet (resolved per-field at commit time); inject
            // it so @context on the root type is visible to @fromContext
            // ancestor lookups.
            let source_data = self
                .cached_query_graph
                .query_graph
                .node_weight(pending.query_graph_node)?;
            if matches!(source_data.type_, QueryGraphNodeType::FederatedRootType(_))
                && let Some(root_type) = state.graph.node(fetch_node).root_type().cloned()
            {
                types = types.pushed(root_type);
            }
            if let Ok(target_type) =
                CompositeTypeDefinitionPosition::try_from(target_node_data.type_.clone())
            {
                types.pushed(target_type)
            } else {
                types
            }
        };
        // Track the parent fetch for @fromContext across entity boundaries:
        // children of an entity root may need to add context selections to
        // the parent fetch that feeds the entity representation.
        let child_context_anchor = if target.entity_root {
            let entity_type =
                CompositeTypeDefinitionPosition::try_from(target_node_data.type_.clone()).ok();
            ContextAnchor {
                fetch: Some(pending.fetch_node),
                op_path: pending.op_path.clone(),
                entity_type,
            }
        } else {
            pending.context_anchor.clone()
        };

        for sub_sel in sub_ss.selections.values().rev().cloned() {
            state.push_pending(
                pending
                    .fork(sub_sel)
                    .at(target_qg_node, fetch_node)
                    .with_op_path(target.op_path.clone())
                    .with_response_path(target.response_path.clone())
                    .with_provides_anchor(child_provides_anchor)
                    .with_defer(child_defer_ref.clone())
                    .with_parent_types(child_parent_types.clone())
                    .with_context_anchor(child_context_anchor.clone()),
            );
        }
        Ok(())
    }

    /// @provides provenance for a committed selection's children (see
    /// [`PendingSelection::provides_anchor`]): an inline fragment whose
    /// downcast leaves the provides-copy layer (copy source, non-copy
    /// target) anchors children at the copy node, keeping its provided-field
    /// edges visible. An interface-level @provides applies to every runtime
    /// type, but only the interface node was copied. Other fragments inherit
    /// the anchor; fields reset it (their children draw on the field's own
    /// target node, which is a copy whenever the field was provided); entity
    /// roots leave the position entirely.
    fn child_provides_anchor(
        &self,
        pending: &PendingSelection,
        target_qg_node: NodeIndex,
        entity_root: bool,
    ) -> Result<Option<NodeIndex>, FederationError> {
        if entity_root || matches!(&pending.selection, Selection::Field(_)) {
            return Ok(None);
        }
        let source_is_copy = self
            .cached_query_graph
            .query_graph
            .node_weight(pending.query_graph_node)?
            .provide_id
            .is_some();
        let target_is_copy = self
            .cached_query_graph
            .query_graph
            .node_weight(target_qg_node)?
            .provide_id
            .is_some();
        Ok(match (source_is_copy, target_is_copy) {
            // Leaving the copy layer: remember where the provided edges live.
            (true, false) => Some(pending.query_graph_node),
            // Inside the copy layer, the node's own edges carry provenance.
            (_, true) => None,
            // Outside it, fragments carry any anchor along unchanged.
            (false, false) => pending.provides_anchor,
        })
    }
}
