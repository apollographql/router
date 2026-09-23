//! Applying a routing choice to the plan state: creating entity fetch
//! groups, wiring dependency edges and entity inputs, and dispatching
//! sub-selections back onto the pending stack.

use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::Name;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use tracing::trace;

use super::super::defer;
use super::super::defer::strip_defer_directive;
use super::super::fetch_graph::InputContribution;
use super::super::fetch_graph::InputRewriteInfo;
use super::super::shared_path::SharedPath;
use super::FieldRoutingSearchSpace;
use super::NodeSource;
use super::RoutingCacheKey;
use super::requires::trailing_condition_fragments;
use super::requires::unconditioned_input_path;
use super::routing::RoutingChoice;
use super::selection_label;
use super::state::CONDITION_DEPTH_LIMIT;
use super::state::PendingSelection;
use super::state::PlanState;
use super::state::TypeNarrowing;
use crate::error::FederationError;
use crate::operation::Field;
use crate::operation::FieldSelection;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::query_graph::QueryGraphEdgeTransition;
use crate::query_graph::QueryGraphNodeType;
use crate::query_graph::graph_path::operation::OpPathElement;

/// (narrowed, before-narrowing) possible runtime types for a child position.
type PossibleTypePair = (Option<Arc<Vec<Name>>>, Option<Arc<Vec<Name>>>);
/// Cross-subgraph intersection filter for fragment conditions.
type IntersectionFilter = Option<Arc<HashSet<Name>>>;
use crate::query_plan::FetchDataPathElement;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;

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
    /// Children start at a fresh entity/root group rather than extending
    /// the current position; @provides provenance does not carry across.
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
        if matches!(choice, RoutingChoice::TypeExplosion) {
            return if self.try_explode_interface_field(state, pending)?
                || self.try_explode_abstract_type(state, pending)?
            {
                Ok(())
            } else {
                Err(FederationError::internal(
                    "type explosion chosen but inapplicable at this position",
                ))
            };
        }

        if matches!(choice, RoutingChoice::StripFragment) {
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

        let qg = &self.query_graph;

        let edge_index = choice
            .edge_index()
            .expect("edge-based routing choice must have an edge index");
        let (_, target_qg_node) = qg.edge_endpoints(edge_index)?;

        // Reject unexpected edge transitions before any mutation. Later
        // failures can leave partial mutations: callers must checkpoint
        // immediately before and roll back on Err.
        let Some(response_path_elements) =
            self.response_path_for_edge(edge_index, &pending.selection)?
        else {
            return Err(FederationError::internal(format!(
                "unexpected edge transition committing {}",
                selection_label(&pending.selection),
            )));
        };

        // Mutating half: commit the hop or resolve the direct fetch group.
        let (fetch_node, key_hop_edge, is_defer_redirect) = match choice {
            RoutingChoice::Provides(_) | RoutingChoice::Local(_) => {
                let node = self.direct_fetch_node(state, pending, choice)?;
                // A deferred field whose enclosing group belongs to a
                // different defer scope needs its own entity fetch, even
                // when no key hop is involved. This lets the executor
                // stream the deferred payload in a separate chunk.
                let enclosing_defer = &state.graph.node(node).defer_ref;
                if pending.defer_ref != *enclosing_defer {
                    let (group, edge) = self.commit_defer_redirect(state, pending, choice)?;
                    (group, Some(edge), true)
                } else {
                    (node, None, false)
                }
            }
            RoutingChoice::RootHop(_) => {
                let (group, hop_edge) = self.commit_root_hop(state, pending, choice)?;
                (group, Some(hop_edge), false)
            }
            _ => {
                let (group, hop_edge) = self.commit_key_hop(state, pending, choice)?;
                (group, Some(hop_edge), false)
            }
        };

        // Pure half: assemble op and response paths for children.
        let mut target = if is_defer_redirect {
            self.defer_redirect_target(pending, fetch_node, response_path_elements)?
        } else {
            self.target_paths(pending, choice, fetch_node, response_path_elements)?
        };
        let ctx = CommitCtx {
            pending,
            choice,
            key_hop_edge,
        };
        let edge = qg.edge_weight(
            choice
                .edge_index()
                .expect("commit called on non-edge choice"),
        )?;
        if let Some(requires_conditions) = &edge.conditions {
            target = self.apply_requires(state, &ctx, requires_conditions, target)?;
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
        let qg = &self.query_graph;
        trace!(
            target_subgraph = %choice.target_subgraph(),
            "committing root type resolution hop",
        );
        let source = self.node_source(pending.query_graph_node)?;
        self.append_typename(state, pending.fetch_node, &pending.op_path, &source);

        let merge_at = self.pending_merge_at(state, pending);

        let (field_source, _) =
            qg.edge_endpoints(choice.edge_index().expect("edge-based choice"))?;
        let field_source_node = qg.node_weight(field_source)?;
        let root_type: CompositeTypeDefinitionPosition =
            field_source_node.type_.clone().try_into()?;
        // The hop resolves through the kind of the subgraph root it lands
        // on, not the surrounding operation's kind.
        let root_kind = self
            .subgraph_root_kind(choice.target_subgraph(), &root_type)?
            .ok_or_else(|| {
                FederationError::internal(format!(
                    "root hop target type {} is not a root type in subgraph {}",
                    root_type.type_name(),
                    choice.target_subgraph(),
                ))
            })?;

        let new_group = self.root_hop_group_avoiding_cycles(
            state,
            choice.target_subgraph(),
            root_type,
            root_kind,
            merge_at,
            pending.fetch_node,
            pending.ordering_dependent(),
            pending.defer_ref.clone(),
        );

        let edge = match state.graph.find_edge(pending.fetch_node, new_group) {
            Some(existing) => existing,
            None => state
                .graph
                .add_dependency(pending.fetch_node, new_group, Vec::new()),
        };

        Ok((new_group, edge))
    }

    /// The root kind `type_pos` serves as in `subgraph`, if it is a root type.
    fn subgraph_root_kind(
        &self,
        subgraph: &Arc<str>,
        type_pos: &CompositeTypeDefinitionPosition,
    ) -> Result<Option<SchemaRootDefinitionKind>, FederationError> {
        let subgraph_schema = self.query_graph.schema_by_source(subgraph)?;
        Ok([
            SchemaRootDefinitionKind::Query,
            SchemaRootDefinitionKind::Mutation,
            SchemaRootDefinitionKind::Subscription,
        ]
        .into_iter()
        .find(|kind| {
            subgraph_schema
                .schema()
                .root_operation((*kind).into())
                .is_some_and(|name| name == type_pos.type_name())
        }))
    }

    /// Commit a key-resolution hop: creates an entity group in the target
    /// subgraph, wires key inputs and dependency edges.
    pub(super) fn commit_key_hop(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
        choice: &RoutingChoice,
    ) -> Result<(NodeIndex, EdgeIndex), FederationError> {
        let qg = &self.query_graph;
        trace!(
            target_subgraph = %choice.target_subgraph(),
            selection = %selection_label(&pending.selection),
            merge_at = ?self.pending_merge_at(state, pending),
            "committing key hop",
        );
        let source = self.node_source(pending.query_graph_node)?;

        // Every key is an entry key; the parent group outputs whatever
        // enters the first group of the chain (the target's own key when
        // there is no chain).
        let key_info = choice.key()?;
        let intermediate_hops = choice.intermediate_hops();
        let first_key: Option<&Arc<SelectionSet>> = intermediate_hops
            .first()
            .map_or(Some(&key_info.key_conditions), |hop| hop.entry_key.as_ref());

        // `conditions_provided` short-circuits the in-place re-derivation:
        // enumeration already verified, against the query graph at this
        // exact position, that an ancestor's @provides makes every key
        // field available here (the subgraph echoes provided fields),
        // including through downcasts out of the provides-copy layer that
        // `can_resolve_in_place` cannot see.
        let key_locally_resolvable = match first_key {
            Some(key_conditions) => {
                matches!(choice, RoutingChoice::KeyHopWithProvidedKey { .. })
                    || self.can_resolve_in_place(
                        pending.query_graph_node,
                        key_conditions,
                        &source,
                    )?
            }
            None => true,
        };

        // Key fields plus __typename, which identifies the entity type.
        let anchor_key = key_locally_resolvable.then_some(first_key).flatten();
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
        let (first_subgraph, first_dest_node) = match intermediate_hops.first() {
            Some(hop) => (
                &qg.node_weight(hop.target_node)?.source,
                Some(hop.target_node),
            ),
            None => (choice.target_subgraph(), None),
        };

        let new_group = self.entity_group_avoiding_cycles(
            state,
            first_subgraph,
            merge_at.clone(),
            pending.fetch_node,
            pending.ordering_dependent(),
            pending.defer_ref.clone(),
        );

        let key_input = if let Some(key_conditions) = first_key {
            let dest_node = match first_dest_node {
                Some(node) => node,
                None => {
                    qg.edge_endpoints(choice.edge_index().expect("edge-based choice"))?
                        .0
                }
            };
            let dest_type: CompositeTypeDefinitionPosition =
                qg.node_weight(dest_node)?.type_.clone().try_into()?;

            Some(InputContribution::Key {
                source_type_name: source.type_pos.type_name().clone(),
                conditions: key_conditions.clone(),
                rewrite_info: InputRewriteInfo {
                    dest_type,
                    dest_subgraph: first_subgraph.clone(),
                },
            })
        } else {
            None
        };

        let edge = self.wire_key_edge(state, pending.fetch_node, new_group, key_input);

        // Keys the current fetch cannot resolve directly are routed as
        // pending selections; ordering edges to the new group are wired as
        // they commit. Statically circular keys are the exception: routing
        // their conditions would recurse without progress, so the anchor
        // must resolve the whole key itself or the commit fails.
        if !key_locally_resolvable && let Some(key_conditions) = first_key.cloned() {
            if matches!(choice, RoutingChoice::CircularKeyHop { .. }) {
                self.commit_circular_key_conditions(
                    state,
                    pending,
                    &key_conditions,
                    &source,
                    pending.fetch_node,
                    &pending.op_path,
                    new_group,
                )?;
            } else {
                self.push_condition_pendings(state, pending, &key_conditions, new_group)?;
            }
        }

        // Multi-hop key chain: walk through intermediate subgraphs,
        // creating an entity group for each hop that feeds the next.
        if !intermediate_hops.is_empty() {
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
        if !self.can_satisfy(key_conditions, &source.type_pos, &source.schema) {
            return Err(FederationError::internal(format!(
                "circular key conditions unsatisfiable at {}: {}",
                source.type_pos.type_name(),
                key_conditions,
            )));
        }
        self.append_entity_inputs(
            state,
            anchor_fetch,
            anchor_path,
            Some(key_conditions),
            source,
        );
        self.push_condition_pendings(state, pending, key_conditions, new_group)?;
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
        let qg = &self.query_graph;
        let mut prev_group = first_group;
        let mut last = None;

        let intermediate_hops = choice.intermediate_hops();
        for (i, hop) in intermediate_hops.iter().enumerate() {
            let hop_node_data = qg.node_weight(hop.target_node)?;
            let hop_type_pos: CompositeTypeDefinitionPosition =
                hop_node_data.type_.clone().try_into()?;

            // This hop's group must output the key entering the next group:
            // the next hop's entry key, or the target's for the last hop.
            let (next_dest_node, exit_key) = match intermediate_hops.get(i + 1) {
                Some(next) => (next.target_node, next.entry_key.as_ref()),
                None => (
                    qg.edge_endpoints(choice.edge_index().expect("edge-based choice"))?
                        .0,
                    Some(&choice.key()?.key_conditions),
                ),
            };
            let next_subgraph = &qg.node_weight(next_dest_node)?.source;

            let next_group = self.entity_group_avoiding_cycles(
                state,
                next_subgraph,
                merge_at.clone(),
                prev_group,
                pending.ordering_dependent(),
                pending.defer_ref.clone(),
            );

            if let Some(key_conds) = exit_key {
                let hop_schema = qg.schema_by_source(&hop_node_data.source)?.clone();
                let hop_source = NodeSource {
                    type_pos: hop_type_pos.clone(),
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

            let hop_key_input = match exit_key {
                Some(key_conds) => {
                    let dest_type: CompositeTypeDefinitionPosition =
                        qg.node_weight(next_dest_node)?.type_.clone().try_into()?;
                    Some(InputContribution::Key {
                        source_type_name: hop_type_pos.type_name().clone(),
                        conditions: key_conds.clone(),
                        rewrite_info: InputRewriteInfo {
                            dest_type,
                            dest_subgraph: next_subgraph.clone(),
                        },
                    })
                }
                None => None,
            };

            let hop_edge = self.wire_key_edge(state, prev_group, next_group, hop_key_input);
            last = Some((next_group, hop_edge));
            prev_group = next_group;
        }

        last.ok_or_else(|| FederationError::internal("intermediate_key_hops is empty"))
    }

    /// Get or create the entity group for (subgraph, merge_at), falling
    /// back to a fresh group when reuse would create a dependency cycle.
    /// The verdict only holds until an edge is added: callers must wire the
    /// anchor->group edge before any other edge insertion (add_dependency's
    /// debug assert backstops this).
    fn entity_group_avoiding_cycles(
        &self,
        state: &mut PlanState,
        subgraph: &Arc<str>,
        merge_at: Vec<FetchDataPathElement>,
        anchor_fetch: NodeIndex,
        ordering_dependent: Option<NodeIndex>,
        defer_ref: Option<String>,
    ) -> NodeIndex {
        let group = state.graph.get_or_create_entity_group_with_defer(
            subgraph,
            merge_at.clone(),
            defer_ref.clone(),
        );
        if Self::group_reusable(state, group, anchor_fetch, ordering_dependent) {
            return group;
        }
        state
            .graph
            .add_entity_group_with_defer(subgraph, merge_at, defer_ref)
    }

    /// Get or create the root hop group for (subgraph, root_kind, merge_at),
    /// falling back to a fresh group when reuse would create a dependency
    /// cycle.
    #[allow(clippy::too_many_arguments)]
    fn root_hop_group_avoiding_cycles(
        &self,
        state: &mut PlanState,
        subgraph: &Arc<str>,
        root_type: CompositeTypeDefinitionPosition,
        root_kind: SchemaRootDefinitionKind,
        merge_at: Vec<FetchDataPathElement>,
        anchor_fetch: NodeIndex,
        ordering_dependent: Option<NodeIndex>,
        defer_ref: Option<String>,
    ) -> NodeIndex {
        let group = state.graph.get_or_create_root_hop_group(
            subgraph,
            root_type.clone(),
            root_kind,
            merge_at.clone(),
            defer_ref.clone(),
        );
        if Self::group_reusable(state, group, anchor_fetch, ordering_dependent) {
            return group;
        }
        state
            .graph
            .add_root_hop_group(subgraph, root_type, root_kind, merge_at, defer_ref)
    }

    /// Whether an existing group can take a dependency edge from
    /// `anchor_fetch` without creating a cycle, and can still run before an
    /// ordering dependent.
    fn group_reusable(
        state: &PlanState,
        group: NodeIndex,
        anchor_fetch: NodeIndex,
        ordering_dependent: Option<NodeIndex>,
    ) -> bool {
        let conflicts_with_dependent = ordering_dependent
            .is_some_and(|dep| group == dep || state.graph.is_reachable(dep, group));
        !conflicts_with_dependent && !state.graph.is_reachable(group, anchor_fetch)
    }

    /// Find or create the anchor->group dependency edge and attach the key
    /// input. An edge already carrying a key for the input's source type is
    /// left alone.
    fn wire_key_edge(
        &self,
        state: &mut PlanState,
        anchor: NodeIndex,
        group: NodeIndex,
        key_input: Option<InputContribution>,
    ) -> EdgeIndex {
        if let Some(existing_edge) = state.graph.find_edge(anchor, group) {
            if let Some(input) = key_input
                && !state
                    .graph
                    .edge_has_key_input(existing_edge, input.source_type_name())
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

    /// The fetch group a direct (non-hop) choice lands in: fields may
    /// create root groups on demand; inline fragments stay in the current
    /// group.
    /// The fetch group a direct (non-hop) choice lands in: fields may create
    /// root groups on demand ([`Self::field_fetch_node`]); inline fragments
    /// stay in the current group. An @interfaceObject fake downcast
    /// additionally pushes a best-effort concrete-`__typename` pending:
    /// execution needs each object's CONCRETE typename to test the condition,
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
                    .query_graph
                    .edge_weight(choice.edge_index().expect("edge-based choice"))?;
                if matches!(
                    edge.transition,
                    QueryGraphEdgeTransition::InterfaceObjectFakeDownCast { .. }
                ) {
                    // Entity groups already carry __typename in their
                    // incoming representation, so recovery is only needed
                    // when the fake downcast originates from a root group.
                    if matches!(
                        state.graph.node(pending.fetch_node).kind,
                        super::super::fetch_graph::FetchGroupKind::Root { .. }
                            | super::super::fetch_graph::FetchGroupKind::RootHop { .. }
                    ) {
                        self.push_interface_object_typename(state, pending)?;
                    }
                }
                Ok(pending.fetch_node)
            }
        }
    }

    /// @interfaceObject subgraph can only report the interface's typename:
    /// execution needs each object's CONCRETE `__typename` to test the
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
        let qg = &self.query_graph;
        let current_node_data = qg.node_weight(pending.query_graph_node)?;
        if matches!(
            current_node_data.type_,
            QueryGraphNodeType::FederatedRootType(_)
        ) {
            let (field_source, _) =
                qg.edge_endpoints(choice.edge_index().expect("edge-based choice"))?;
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
        let qg = &self.query_graph;
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
        fetch_node: NodeIndex,
        response_path_elements: Vec<FetchDataPathElement>,
    ) -> Result<CommitTarget, FederationError> {
        let qg = &self.query_graph;
        let is_direct = choice.is_direct();
        let op_path = if is_direct {
            // Direct choices extend the current op path with this selection.
            match &pending.selection {
                Selection::Field(field_sel) => pending
                    .op_path
                    .pushed(Arc::new(OpPathElement::Field(field_sel.field.clone()))),
                Selection::InlineFragment(frag_sel) => {
                    let stripped = strip_defer_directive(&frag_sel.inline_fragment);
                    let edge = qg.edge_weight(choice.edge_index().expect("edge-based choice"))?;
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
            }
        } else {
            // Hops restart the op path at the new group's root: empty for
            // root hops; for key hops, `... on <ConcreteType>` (entity
            // fetches start from the _Entity union) plus any trailing
            // @skip/@include fragments.
            let base = if matches!(choice, RoutingChoice::RootHop(_)) {
                SharedPath::new()
            } else {
                let (field_source, _) =
                    qg.edge_endpoints(choice.edge_index().expect("edge-based choice"))?;
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
        };

        // Hops restart the response path at the new fetch node's root.
        // Only direct choices continue from the pending's current position.
        // Type conditions do not extend it, so entity fetches triggered by
        // different type conditions at one response-tree level share
        // identical merge_at paths.
        let response_path = {
            let mut rp = if is_direct {
                self.conditioned_path_in_fetch(pending)
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
            entity_root: !choice.is_direct(),
        })
    }

    /// Route a deferred field into a separate entity group when it lives
    /// in the same subgraph as its enclosing fetch but belongs to a
    /// different defer scope. This creates the same structure as a key hop
    /// so the executor can stream the deferred payload independently.
    fn commit_defer_redirect(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
        choice: &RoutingChoice,
    ) -> Result<(NodeIndex, EdgeIndex), FederationError> {
        let qg = &self.query_graph;
        let source = self.node_source(pending.query_graph_node)?;
        let subgraph = qg.node_weight(pending.query_graph_node)?.source.clone();
        if let Some(root_kind) = self.subgraph_root_kind(&subgraph, &source.type_pos)? {
            return Ok(
                self.commit_root_defer_redirect(state, pending, &subgraph, &source, root_kind)
            );
        }

        // The self-key edge exists specifically for @defer re-entering a
        // subgraph; out_edges filters self-edges so use the unfiltered view.
        let key_conditions = qg
            .out_edges_with_federation_self_edges(pending.query_graph_node)
            .into_iter()
            .find_map(|edge_ref| {
                let edge = edge_ref.weight();
                if !matches!(edge.transition, QueryGraphEdgeTransition::KeyResolution) {
                    return None;
                }
                let target_node = qg.node_weight(edge_ref.target()).ok()?;
                if target_node.source != subgraph {
                    return None;
                }
                edge.conditions.clone()
            });

        self.append_entity_inputs(
            state,
            pending.fetch_node,
            &pending.op_path,
            key_conditions.as_ref(),
            &source,
        );

        let merge_at = self.pending_merge_at(state, pending);
        let new_group = self.entity_group_avoiding_cycles(
            state,
            &subgraph,
            merge_at,
            pending.fetch_node,
            pending.ordering_dependent(),
            pending.defer_ref.clone(),
        );

        let (field_source, _) =
            qg.edge_endpoints(choice.edge_index().expect("edge-based choice"))?;
        let dest_type: CompositeTypeDefinitionPosition =
            qg.node_weight(field_source)?.type_.clone().try_into()?;

        let key_input = key_conditions.map(|conditions| InputContribution::Key {
            source_type_name: source.type_pos.type_name().clone(),
            conditions,
            rewrite_info: InputRewriteInfo {
                dest_type,
                dest_subgraph: subgraph,
            },
        });

        let edge = self.wire_key_edge(state, pending.fetch_node, new_group, key_input);
        Ok((new_group, edge))
    }

    /// Root types have no key to re-enter through, so a deferred field on a
    /// subgraph root re-enters it with a root hop in the pending's scope.
    fn commit_root_defer_redirect(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
        subgraph: &Arc<str>,
        source: &NodeSource,
        root_kind: SchemaRootDefinitionKind,
    ) -> (NodeIndex, EdgeIndex) {
        self.append_typename(state, pending.fetch_node, &pending.op_path, source);
        let merge_at = self.pending_merge_at(state, pending);
        let new_group = self.root_hop_group_avoiding_cycles(
            state,
            subgraph,
            source.type_pos.clone(),
            root_kind,
            merge_at,
            pending.fetch_node,
            pending.ordering_dependent(),
            pending.defer_ref.clone(),
        );
        let edge = match state.graph.find_edge(pending.fetch_node, new_group) {
            Some(existing) => existing,
            None => state
                .graph
                .add_dependency(pending.fetch_node, new_group, Vec::new()),
        };
        (new_group, edge)
    }

    /// Build a commit target for a same-subgraph defer redirect. The field
    /// is routed to a new entity group, so its op_path restarts from the
    /// entity root (like a key hop) rather than extending the parent path.
    fn defer_redirect_target(
        &self,
        pending: &PendingSelection,
        fetch_node: NodeIndex,
        response_path_elements: Vec<FetchDataPathElement>,
    ) -> Result<CommitTarget, FederationError> {
        let node_data = self.query_graph.node_weight(pending.query_graph_node)?;
        let type_pos = CompositeTypeDefinitionPosition::try_from(node_data.type_.clone())?;
        let mut op_path = if self
            .subgraph_root_kind(&node_data.source, &type_pos)?
            .is_some()
        {
            // Root redirects start at the hop's root, like a root hop.
            SharedPath::new()
        } else {
            let mut path = self.entity_root_path(type_pos.type_name())?;
            for element in trailing_condition_fragments(&pending.op_path) {
                path = path.pushed(element);
            }
            path
        };
        let op_element: Arc<OpPathElement> = match &pending.selection {
            Selection::Field(field_sel) => Arc::new(OpPathElement::Field(field_sel.field.clone())),
            Selection::InlineFragment(frag_sel) => Arc::new(OpPathElement::InlineFragment(
                frag_sel.inline_fragment.clone(),
            )),
        };
        op_path = op_path.pushed(op_element);

        let mut response_path = SharedPath::new();
        for element in response_path_elements {
            response_path = response_path.pushed(element);
        }

        Ok(CommitTarget {
            fetch_node,
            op_path,
            response_path,
            entity_root: true,
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
        merge_at.extend(self.conditioned_path_in_fetch(pending).iter().cloned());
        merge_at
    }

    /// Sorted possible runtime type names of a composite type in the
    /// supergraph schema.
    fn possible_type_names(
        &self,
        ty: &CompositeTypeDefinitionPosition,
    ) -> Result<Arc<Vec<Name>>, FederationError> {
        let mut names: Vec<Name> = self
            .supergraph_schema
            .possible_runtime_types(ty.clone())?
            .into_iter()
            .map(|pos| pos.type_name)
            .collect();
        names.sort();
        Ok(Arc::new(names))
    }

    /// Possible-types tracking for a committed selection's children: a field
    /// resets both sets to its output type's runtime types; a typed inline
    /// fragment narrows the current set; a condition-only fragment inherits.
    fn child_possible_types(
        &self,
        pending: &PendingSelection,
    ) -> Result<PossibleTypePair, FederationError> {
        match &pending.selection {
            Selection::Field(field_sel) => {
                let field_def = field_sel
                    .field
                    .field_position
                    .get(field_sel.field.schema.schema())?;
                let Ok(ty) = self
                    .supergraph_schema
                    .get_type(field_def.ty.inner_named_type())
                else {
                    return Ok((None, None));
                };
                let Ok(pos) = CompositeTypeDefinitionPosition::try_from(ty) else {
                    return Ok((None, None));
                };
                let names = self.possible_type_names(&pos)?;
                Ok((Some(names.clone()), Some(names)))
            }
            Selection::InlineFragment(frag_sel) => {
                let Some(cond) = &frag_sel.inline_fragment.type_condition_position else {
                    return Ok((
                        pending.narrowing.possible_types.clone(),
                        pending.narrowing.possible_types_after_last_field.clone(),
                    ));
                };
                let cond_names = self.possible_type_names(cond)?;
                let narrowed = match &pending.narrowing.possible_types {
                    Some(parent) => Arc::new(
                        parent
                            .iter()
                            .filter(|n| cond_names.contains(n))
                            .cloned()
                            .collect::<Vec<_>>(),
                    ),
                    None => cond_names,
                };
                Ok((
                    Some(narrowed),
                    pending.narrowing.possible_types_after_last_field.clone(),
                ))
            }
        }
    }

    /// `pending.path_in_fetch`, with the narrowed possible-type set attached
    /// to its last element when fragments since the nearest enclosing field
    /// narrowed that field's output types, so fetches under different
    /// abstract branches merge at distinct, discriminated paths.
    fn conditioned_path_in_fetch(
        &self,
        pending: &PendingSelection,
    ) -> SharedPath<FetchDataPathElement> {
        let (Some(possible), Some(after_field)) = (
            &pending.narrowing.possible_types,
            &pending.narrowing.possible_types_after_last_field,
        ) else {
            return pending.path_in_fetch.clone();
        };
        if possible.len() == after_field.len() {
            return pending.path_in_fetch.clone();
        }
        let mut elements: Vec<FetchDataPathElement> =
            pending.path_in_fetch.iter().cloned().collect();
        let conditioned = match elements.pop() {
            Some(FetchDataPathElement::Key(name, _)) => {
                FetchDataPathElement::Key(name, Some(possible.as_ref().clone()))
            }
            Some(FetchDataPathElement::AnyIndex(_)) => {
                FetchDataPathElement::AnyIndex(Some(possible.as_ref().clone()))
            }
            Some(other) => other,
            None => return pending.path_in_fetch.clone(),
        };
        elements.push(conditioned);
        SharedPath::from_vec(elements)
    }

    /// When the committed field returns an inconsistent abstract type (its
    /// runtime members differ per subgraph) and the routing path here
    /// includes a shareable fork (the field or an ancestor had routing
    /// options in multiple subgraphs), child fragments must be restricted to
    /// the cross-subgraph type intersection.
    fn intersection_filter_for_field(
        &self,
        pending: &PendingSelection,
        target_node: NodeIndex,
    ) -> Result<IntersectionFilter, FederationError> {
        if !pending.narrowing.shareable_path {
            return Ok(None);
        }
        let target_data = self.query_graph.node_weight(target_node)?;
        let Ok(target_pos) = CompositeTypeDefinitionPosition::try_from(target_data.type_.clone())
        else {
            return Ok(None);
        };
        let target_subgraph = &target_data.source;
        Ok(self.allowed_inconsistent_members(target_pos.type_name(), target_subgraph))
    }

    /// True when this field has routing options in more than one subgraph
    /// from the current position (direct edge plus key hops, or key hops to
    /// multiple distinct subgraphs). Only called for non-root nodes:
    /// `child_shareability` resolves FederatedRootType positions to
    /// non-shareable before reaching here.
    fn field_is_shareable_here(
        &self,
        field_sel: &FieldSelection,
        source_node: NodeIndex,
    ) -> Result<bool, FederationError> {
        let field = &field_sel.field;
        let has_direct = self.edge_for_field(source_node, field).is_some();
        let field_name = field.field_position.field_name();
        let hops = self.key_hops_guarded(
            source_node,
            RoutingCacheKey::Field(field_name.clone()),
            |target| self.edge_for_field(target, field),
        )?;
        if has_direct && !hops.is_empty() {
            return Ok(true);
        }
        if hops.len() > 1 {
            let mut subgraphs = HashSet::new();
            for hop in hops.iter() {
                subgraphs.insert(hop.target_subgraph().clone());
            }
            return Ok(subgraphs.len() > 1);
        }
        Ok(false)
    }

    /// Whether this field is defined in the parent type in more than one
    /// subgraph schema (ignoring routing reachability). @external does not
    /// count as availability: the subgraph cannot resolve the field, so it
    /// creates no fork.
    fn field_in_multiple_subgraphs(
        &self,
        field_sel: &FieldSelection,
        source_node: NodeIndex,
    ) -> Result<bool, FederationError> {
        let source_data = self.query_graph.node_weight(source_node)?;
        let Ok(source_pos) = CompositeTypeDefinitionPosition::try_from(source_data.type_.clone())
        else {
            return Ok(false);
        };
        let parent_type_name = source_pos.type_name();
        let field_name = field_sel.field.field_position.field_name();
        let mut count = 0u32;
        for (_source, schema) in self.query_graph.subgraph_schemas() {
            let Ok(parent_type) = schema.get_type(parent_type_name) else {
                continue;
            };
            let Ok(composite): Result<CompositeTypeDefinitionPosition, _> = parent_type.try_into()
            else {
                continue;
            };
            let Ok(field_pos) = composite.field(field_name.clone()) else {
                continue;
            };
            if field_pos.get(schema.schema()).is_err() {
                continue;
            }
            if schema
                .subgraph_metadata()
                .is_some_and(|meta| meta.is_field_external(&field_pos))
            {
                continue;
            }
            count += 1;
            if count > 1 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Shareable-fork propagation for children: whether some field on this
    /// pending's path had routing options in multiple subgraphs, and the
    /// intersection filter that follows.
    fn child_shareability(
        &self,
        pending: &PendingSelection,
        target_qg_node: NodeIndex,
    ) -> Result<(bool, IntersectionFilter), FederationError> {
        let shareable = if let Selection::Field(field_sel) = &pending.selection {
            let source_data = self.query_graph.node_weight(pending.query_graph_node)?;
            if matches!(source_data.type_, QueryGraphNodeType::FederatedRootType(_)) {
                // Root-level shareability is resolved when BULB commits to a
                // subgraph; only key-hop-based shareability creates
                // unresolved forks needing the intersection filter.
                false
            } else {
                self.field_is_shareable_here(field_sel, pending.query_graph_node)?
                    || (pending.narrowing.shareable_path
                        && self.field_in_multiple_subgraphs(field_sel, pending.query_graph_node)?)
            }
        } else {
            pending.narrowing.shareable_path
        };

        let filter = if shareable && matches!(&pending.selection, Selection::Field(_)) {
            self.intersection_filter_for_field(pending, target_qg_node)?
        } else {
            None
        };
        Ok((shareable, filter))
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
        // Conditions resolve in the anchor group's defer scope, not the
        // dependent's: keys for a deferred fetch merge into the fetches the
        // enclosing scope already makes instead of duplicating them in the
        // deferred section.
        let anchor_defer = state.graph.node(anchor.fetch_node).defer_ref.clone();
        for sel in conditions.selections.values().rev().cloned() {
            let mut forked = anchor.fork(sel).into_condition_for(dependent);
            forked.op_path = input_path.clone();
            forked.defer_ref = anchor_defer.clone();
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
            let target_data = self.query_graph.node_weight(target_qg_node)?;
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

        let (child_shareable_path, child_intersection_filter) =
            self.child_shareability(pending, target_qg_node)?;
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

        let (child_possible, child_after_field) = self.child_possible_types(pending)?;
        // A typed fragment whose runtime-type intersection with its position
        // is empty matches no objects — dead code (e.g. a shared interface
        // fragment under sibling unions with disjoint members). Route
        // nothing under it.
        if matches!(&pending.selection, Selection::InlineFragment(_))
            && child_possible.as_ref().is_some_and(|p| p.is_empty())
        {
            return Ok(());
        }
        let child_narrowing = TypeNarrowing {
            shareable_path: child_shareable_path,
            intersection_filter: child_intersection_filter,
            possible_types: child_possible,
            possible_types_after_last_field: child_after_field,
        };
        // When the committed selection is an inline fragment carrying
        // @defer, extract the label and propagate it to children so fetch
        // nodes created downstream land in the deferred partition.
        let child_defer_ref = defer::defer_context(&pending.selection)
            .0
            .or_else(|| pending.defer_ref.clone());

        for sub_sel in sub_ss.selections.values().rev().cloned() {
            state.push_pending(
                pending
                    .fork(sub_sel)
                    .at(target_qg_node, fetch_node)
                    .with_op_path(target.op_path.clone())
                    .with_response_path(target.response_path.clone())
                    .with_provides_anchor(child_provides_anchor)
                    .with_defer(child_defer_ref.clone())
                    .with_narrowing(child_narrowing.clone()),
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
    /// target node, which IS a copy whenever the field was provided); entity
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
            .query_graph
            .node_weight(pending.query_graph_node)?
            .provide_id
            .is_some();
        let target_is_copy = self
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
