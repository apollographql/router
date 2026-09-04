use std::sync::Arc;

use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;
use tracing::trace;

use super::FieldRoutingSearchSpace;
use super::state::PendingSelection;
use crate::error::FederationError;
use crate::operation::FieldSelection;
use crate::operation::InlineFragmentSelection;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::operation::TYPENAME_FIELD;
use crate::query_graph::QueryGraphEdgeTransition;
use crate::query_graph::QueryGraphNodeType;
use crate::schema::position::CompositeTypeDefinitionPosition;

const MAX_CHAIN_DEPTH: usize = 5;

/// Where a routing choice sends the field.
#[derive(Clone, Debug)]
pub(crate) enum RoutingTarget {
    /// Standard subgraph field resolution via a query graph edge.
    SubgraphEdge {
        edge_index: EdgeIndex,
        target_subgraph: Arc<str>,
    },
}

/// A routing choice for a field: either a subgraph edge or a key hop.
#[derive(Clone, Debug)]
pub(crate) struct RoutingChoice {
    /// Where to route the field.
    pub(crate) target: RoutingTarget,
    /// How this choice reaches its target subgraph.
    pub(crate) hop_kind: HopKind,
    /// @key fields to include in parent fetch (if key hop required).
    pub(crate) key_conditions: Option<Arc<SelectionSet>>,
    /// Whether the current subgraph can satisfy the key conditions without
    /// an intermediate hop.
    pub(crate) conditions_locally_satisfiable: bool,
    /// When the routed edge carries @requires conditions: whether the fetch
    /// anchoring the field can select the condition fields in place. Commit
    /// applies this verdict instead of re-deriving it. True when the edge
    /// has no conditions.
    pub(crate) requires_resolvable_in_place: bool,
    /// When the field is only reachable through a multi-hop key chain,
    /// this captures the intermediate hops in order. Commit creates a
    /// chained sequence of entity groups, one per intermediate, before
    /// the final hop that resolves the field.
    pub(crate) intermediate_key_hops: Vec<IntermediateKeyHop>,
}

/// One leg of a multi-hop key chain: a key resolution that lands at a
/// node which doesn't have the target field but bridges toward one that
/// does. `key_conditions` is the exit key, the fields this hop's group
/// must output to key into the next group.
#[derive(Clone, Debug)]
pub(crate) struct IntermediateKeyHop {
    pub(crate) target_subgraph: Arc<str>,
    pub(crate) key_conditions: Option<Arc<SelectionSet>>,
    pub(crate) target_node: NodeIndex,
}

/// How a routing choice reaches its target subgraph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HopKind {
    /// The current fetch group's subgraph resolves the selection in place.
    Direct,
    /// Entity fetch (`_entities`) in another subgraph, reached via @key.
    KeyHop,
    /// Root-level fetch at a non-root path (RootTypeResolution).
    RootHop,
}

impl RoutingChoice {
    /// A direct edge in the current subgraph.
    fn direct(edge_index: EdgeIndex, target_subgraph: Arc<str>) -> Self {
        Self {
            target: RoutingTarget::SubgraphEdge {
                edge_index,
                target_subgraph,
            },
            hop_kind: HopKind::Direct,
            key_conditions: None,
            conditions_locally_satisfiable: true,
            requires_resolvable_in_place: true,
            intermediate_key_hops: Vec::new(),
        }
    }

    /// A same-subgraph entity re-entry resolving a field whose @requires
    /// conditions cannot simply sit alongside it.
    fn self_key_hop(
        edge_index: EdgeIndex,
        target_subgraph: Arc<str>,
        key: Arc<SelectionSet>,
        requires_resolvable_in_place: bool,
    ) -> Self {
        Self {
            target: RoutingTarget::SubgraphEdge {
                edge_index,
                target_subgraph,
            },
            hop_kind: HopKind::KeyHop,
            key_conditions: Some(key),
            conditions_locally_satisfiable: true,
            requires_resolvable_in_place,
            intermediate_key_hops: Vec::new(),
        }
    }

    /// Target subgraph name.
    pub(crate) fn target_subgraph(&self) -> &Arc<str> {
        match &self.target {
            RoutingTarget::SubgraphEdge {
                target_subgraph, ..
            } => target_subgraph,
        }
    }

    /// Query graph edge index.
    pub(crate) fn edge_index(&self) -> EdgeIndex {
        match &self.target {
            RoutingTarget::SubgraphEdge { edge_index, .. } => *edge_index,
        }
    }
}

/// Rank of a routing option, best first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RoutingPreference {
    Provides,
    DirectLocal,
    /// Same-subgraph entity re-entry for in-place-unresolvable @requires.
    SelfRequiresHop,
    LocallySatisfiableKeyHop,
    RemoteKeyHop,
    /// A 2+ hop key chain. Costs multiple entity fetches, so it ranks
    /// below single remote hops.
    ChainedKeyHop,
}

struct KeyHopCandidate {
    found_edge_idx: EdgeIndex,
    target_subgraph: Arc<str>,
    is_root: bool,
    conditions_local: bool,
    key_conditions: Option<Arc<SelectionSet>>,
    key_leaf_count: usize,
}

impl KeyHopCandidate {
    /// Locally satisfiable keys must never lose the per-subgraph dedup to a
    /// cheaper-looking unsatisfiable key.
    fn dedup_rank(&self) -> (u8, usize) {
        let satisfiability = if self.conditions_local { 0 } else { 2 };
        (satisfiability, self.key_leaf_count)
    }

    fn insert_or_replace(candidates: &mut Vec<Self>, candidate: Self) {
        if let Some(existing) = candidates
            .iter_mut()
            .find(|c| c.target_subgraph == candidate.target_subgraph)
        {
            if candidate.dedup_rank() < existing.dedup_rank() {
                *existing = candidate;
            }
        } else {
            candidates.push(candidate);
        }
    }
}

impl FieldRoutingSearchSpace {
    /// Whether the @requires conditions on `edge_idx` (if any) can be
    /// selected in place at `node`.
    fn requires_conditions_resolvable_in_place(
        &self,
        node: NodeIndex,
        edge_idx: EdgeIndex,
    ) -> Result<bool, FederationError> {
        let edge = self.query_graph.edge_weight(edge_idx)?;
        let Some(conditions) = &edge.conditions else {
            return Ok(true);
        };
        let source = self.node_source(node)?;
        self.can_resolve_in_place(node, conditions, &source)
    }

    /// Enumerate local resolution strategies for a direct edge carrying
    /// @requires conditions.
    fn push_requires_strategy_options(
        &self,
        options: &mut Vec<RoutingChoice>,
        node: NodeIndex,
        edge_idx: EdgeIndex,
        target_subgraph: &Arc<str>,
    ) -> Result<(), FederationError> {
        let in_place = self.requires_conditions_resolvable_in_place(node, edge_idx)?;
        let key = self.query_graph.get_locally_satisfiable_key(node)?;
        if in_place {
            options.push(RoutingChoice::direct(edge_idx, target_subgraph.clone()));
        }
        if let Some(key) = key {
            options.push(RoutingChoice::self_key_hop(
                edge_idx,
                target_subgraph.clone(),
                Arc::new(key),
                in_place,
            ));
        }
        Ok(())
    }

    pub(super) fn key_hop_options(
        &self,
        pending_node: NodeIndex,
        edge_finder: impl Fn(NodeIndex) -> Option<EdgeIndex>,
    ) -> Result<Vec<RoutingChoice>, FederationError> {
        let current_node = self.query_graph.node_weight(pending_node)?;
        let current_source = current_node.source.clone();
        let source_type: Option<CompositeTypeDefinitionPosition> =
            current_node.type_.clone().try_into().ok();
        let source_schema = self.query_graph.schema_by_source(&current_source).ok();

        let mut candidates: Vec<KeyHopCandidate> = Vec::new();
        let mut need_chain: Vec<(NodeIndex, EdgeIndex)> = Vec::new();

        for key_edge_idx in self.out_edge_indices(pending_node) {
            let key_edge = self.query_graph.edge_weight(key_edge_idx)?;
            if !matches!(
                key_edge.transition,
                QueryGraphEdgeTransition::KeyResolution
                    | QueryGraphEdgeTransition::RootTypeResolution { .. }
            ) {
                continue;
            }
            let (_, key_target) = self.query_graph.edge_endpoints(key_edge_idx)?;
            let key_target_node = self.query_graph.node_weight(key_target)?;
            if key_target_node.source == current_source {
                continue;
            }
            if let Some(found_edge_idx) = edge_finder(key_target) {
                let candidate = self.single_hop_candidate(
                    pending_node,
                    found_edge_idx,
                    key_edge,
                    key_target_node.source.clone(),
                    (&current_source, &source_type, &source_schema),
                )?;
                KeyHopCandidate::insert_or_replace(&mut candidates, candidate);
            } else if !matches!(
                key_edge.transition,
                QueryGraphEdgeTransition::RootTypeResolution { .. }
            ) {
                need_chain.push((key_target, key_edge_idx));
            }
        }

        let mut options = self.evaluate_hop_candidates(pending_node, &candidates)?;

        // Only explore chained key hops when no committable single-hop
        // reached the field; chained hops are strictly more expensive.
        if options.is_empty() {
            let single_hop_count = options.len();
            for (key_target, key_edge_idx) in need_chain {
                let key_edge = self.query_graph.edge_weight(key_edge_idx)?;
                for option in self.chained_key_hop_options(
                    pending_node,
                    key_target,
                    key_edge,
                    &current_source,
                    &source_type,
                    &source_schema,
                    &edge_finder,
                )? {
                    options.push(option);
                }
                if options.len() > single_hop_count {
                    break;
                }
            }
        }
        Ok(options)
    }

    /// Follow key edges transitively from a node that doesn't have the
    /// target field, searching for a node that does. Produces multi-hop
    /// routing choices whose `intermediate_key_hops` list captures the
    /// full chain.
    #[allow(clippy::too_many_arguments)]
    fn chained_key_hop_options(
        &self,
        origin_node: NodeIndex,
        first_intermediate: NodeIndex,
        first_key_edge: &crate::query_graph::QueryGraphEdge,
        origin_source: &Arc<str>,
        origin_type: &Option<CompositeTypeDefinitionPosition>,
        origin_schema: &Option<&crate::schema::ValidFederationSchema>,
        edge_finder: &impl Fn(NodeIndex) -> Option<EdgeIndex>,
    ) -> Result<Vec<RoutingChoice>, FederationError> {
        let first_conditions_local = match (&first_key_edge.conditions, origin_type, origin_schema)
        {
            (Some(conds), Some(st), Some(ss)) => self.can_satisfy(conds, st, origin_source, ss),
            (None, _, _) => true,
            _ => false,
        };

        let first_intermediate_data = self.query_graph.node_weight(first_intermediate)?;
        let first_hop = IntermediateKeyHop {
            target_subgraph: first_intermediate_data.source.clone(),
            key_conditions: first_key_edge.conditions.clone(),
            target_node: first_intermediate,
        };

        let mut visited: Vec<Arc<str>> = vec![
            origin_source.clone(),
            first_intermediate_data.source.clone(),
        ];

        let mut frontier: Vec<(NodeIndex, Vec<IntermediateKeyHop>)> =
            vec![(first_intermediate, vec![first_hop])];
        let mut options = Vec::new();

        while let Some((current, path)) = frontier.pop() {
            if path.len() >= MAX_CHAIN_DEPTH {
                continue;
            }

            for key_edge_idx in self.out_edge_indices(current) {
                let key_edge = self.query_graph.edge_weight(key_edge_idx)?;
                if !matches!(key_edge.transition, QueryGraphEdgeTransition::KeyResolution) {
                    continue;
                }
                let (_, next_target) = self.query_graph.edge_endpoints(key_edge_idx)?;
                let next_target_data = self.query_graph.node_weight(next_target)?;
                if visited.contains(&next_target_data.source) {
                    continue;
                }
                visited.push(next_target_data.source.clone());

                if let Some(found_edge_idx) = edge_finder(next_target) {
                    let mut hops = path.clone();
                    // Shift exit keys: each hop carries the conditions the next
                    // group needs to enter, not the entry conditions this hop
                    // was reached by.
                    for i in 0..hops.len() - 1 {
                        hops[i].key_conditions = hops[i + 1].key_conditions.clone();
                    }
                    let last = hops.last_mut().unwrap();
                    last.key_conditions = key_edge.conditions.clone();

                    trace!(
                        chain_length = hops.len(),
                        final_subgraph = %next_target_data.source,
                        "found field via {}-hop key chain",
                        hops.len() + 1,
                    );

                    options.push(RoutingChoice {
                        target: RoutingTarget::SubgraphEdge {
                            edge_index: found_edge_idx,
                            target_subgraph: next_target_data.source.clone(),
                        },
                        hop_kind: HopKind::KeyHop,
                        key_conditions: first_key_edge.conditions.clone(),
                        conditions_locally_satisfiable: first_conditions_local,
                        requires_resolvable_in_place: self
                            .requires_conditions_resolvable_in_place(origin_node, found_edge_idx)?,
                        intermediate_key_hops: hops,
                    });
                } else {
                    let mut extended = path.clone();
                    extended.push(IntermediateKeyHop {
                        target_subgraph: next_target_data.source.clone(),
                        key_conditions: key_edge.conditions.clone(),
                        target_node: next_target,
                    });
                    frontier.push((next_target, extended));
                }
            }
            if !options.is_empty() {
                break;
            }
        }
        Ok(options)
    }

    /// Build the dedup candidate for a single-hop key edge whose target has
    /// the field. Satisfiability is computed here, before dedup, so a key
    /// the state can produce is never collapsed into an unsatisfiable
    /// same-subgraph rival.
    fn single_hop_candidate(
        &self,
        _pending_node: NodeIndex,
        found_edge_idx: EdgeIndex,
        key_edge: &crate::query_graph::QueryGraphEdge,
        target_subgraph: Arc<str>,
        (current_source, source_type, source_schema): (
            &Arc<str>,
            &Option<CompositeTypeDefinitionPosition>,
            &Option<&crate::schema::ValidFederationSchema>,
        ),
    ) -> Result<KeyHopCandidate, FederationError> {
        let is_root = matches!(
            key_edge.transition,
            QueryGraphEdgeTransition::RootTypeResolution { .. }
        );
        let conditions_local = if is_root {
            true
        } else {
            match (&key_edge.conditions, source_type, source_schema) {
                (Some(conds), Some(st), Some(ss)) => {
                    self.can_satisfy(conds, st, current_source, ss)
                }
                (None, _, _) => true,
                _ => false,
            }
        };
        let key_leaf_count = key_edge
            .conditions
            .as_ref()
            .map(|c| selection_leaf_count(c))
            .unwrap_or(0);
        Ok(KeyHopCandidate {
            found_edge_idx,
            target_subgraph,
            is_root,
            conditions_local,
            key_conditions: key_edge.conditions.clone(),
            key_leaf_count,
        })
    }

    fn evaluate_hop_candidates(
        &self,
        pending_node: NodeIndex,
        candidates: &[KeyHopCandidate],
    ) -> Result<Vec<RoutingChoice>, FederationError> {
        let mut options = Vec::with_capacity(candidates.len());
        for c in candidates {
            let hop_kind = if c.is_root {
                HopKind::RootHop
            } else {
                HopKind::KeyHop
            };
            trace!(
                target_subgraph = %c.target_subgraph,
                conditions_local = c.conditions_local,
                ?hop_kind,
                "found edge via key hop",
            );
            options.push(RoutingChoice {
                target: RoutingTarget::SubgraphEdge {
                    edge_index: c.found_edge_idx,
                    target_subgraph: c.target_subgraph.clone(),
                },
                hop_kind,
                key_conditions: c.key_conditions.clone(),
                conditions_locally_satisfiable: c.conditions_local,
                requires_resolvable_in_place: self
                    .requires_conditions_resolvable_in_place(pending_node, c.found_edge_idx)?,
                intermediate_key_hops: Vec::new(),
            });
        }
        Ok(options)
    }

    /// Enumerate valid `RoutingChoice`s for a pending selection from its
    /// current query graph node, covering same-subgraph resolution and
    /// cross-subgraph key hops.
    pub(super) fn routing_options(
        &self,
        pending: &PendingSelection,
    ) -> Result<Vec<RoutingChoice>, FederationError> {
        let current_node_data = self.query_graph.node_weight(pending.query_graph_node)?;
        let options = if matches!(
            current_node_data.type_,
            QueryGraphNodeType::FederatedRootType(_)
        ) {
            self.federated_root_options(pending)?
        } else {
            let mut options = match &pending.selection {
                Selection::Field(field_selection) => {
                    self.field_options(pending, field_selection)?
                }
                Selection::InlineFragment(fragment_selection) => {
                    self.fragment_options(pending, fragment_selection)?
                }
            };
            self.rank_options(&mut options);
            options
        };
        Ok(options)
    }

    /// Options at the FederatedRootType head node, which fans out to
    /// per-subgraph roots via SubgraphEnteringTransition edges.
    pub(super) fn federated_root_options(
        &self,
        pending: &PendingSelection,
    ) -> Result<Vec<RoutingChoice>, FederationError> {
        let mut options = Vec::new();
        let Selection::Field(field_selection) = &pending.selection else {
            return Ok(options);
        };
        for entry_edge_idx in self.out_edge_indices(pending.query_graph_node) {
            let entry_edge = self.query_graph.edge_weight(entry_edge_idx)?;
            if !matches!(
                entry_edge.transition,
                QueryGraphEdgeTransition::SubgraphEnteringTransition
            ) {
                continue;
            }
            let (_, subgraph_root) = self.query_graph.edge_endpoints(entry_edge_idx)?;
            if let Some(field_edge_idx) = self.edge_for_field(subgraph_root, &field_selection.field)
            {
                let subgraph_node = self.query_graph.node_weight(subgraph_root)?;
                options.push(RoutingChoice::direct(
                    field_edge_idx,
                    subgraph_node.source.clone(),
                ));
            }
        }

        // Rank root options by locally-resolvable sub-selection count.
        if options.len() > 1
            && let Some(sub_ss) = field_selection.selection_set.as_ref()
        {
            options.sort_by(|a, b| {
                let count_a = self
                    .query_graph
                    .edge_endpoints(a.edge_index())
                    .ok()
                    .map(|(_, target)| self.count_local_sub_selections(target, sub_ss))
                    .unwrap_or(0);
                let count_b = self
                    .query_graph
                    .edge_endpoints(b.edge_index())
                    .ok()
                    .map(|(_, target)| self.count_local_sub_selections(target, sub_ss))
                    .unwrap_or(0);
                count_b.cmp(&count_a)
            });
        }

        Ok(options)
    }

    /// Options for a field selection: the direct edge (if any) plus every
    /// cross-subgraph key hop. Hops are enumerated even when a viable direct
    /// edge exists, because hopping early can be cheaper than hopping per-child
    /// later.
    pub(super) fn field_options(
        &self,
        pending: &PendingSelection,
        field_selection: &FieldSelection,
    ) -> Result<Vec<RoutingChoice>, FederationError> {
        let mut options = Vec::new();
        if let Some(edge_idx) =
            self.edge_for_field(pending.query_graph_node, &field_selection.field)
        {
            let (_, target) = self.query_graph.edge_endpoints(edge_idx)?;
            let target_node = self.query_graph.node_weight(target)?;
            let edge = self.query_graph.edge_weight(edge_idx)?;
            if edge.conditions.is_none() {
                options.push(RoutingChoice::direct(edge_idx, target_node.source.clone()));
            } else {
                self.push_requires_strategy_options(
                    &mut options,
                    pending.query_graph_node,
                    edge_idx,
                    &target_node.source,
                )?;
            }
        }

        let hops = self.key_hop_options(pending.query_graph_node, |key_target| {
            self.edge_for_field(key_target, &field_selection.field)
        })?;
        options.extend(hops);

        Ok(options)
    }

    /// Options for an inline fragment: a downcast edge when one exists,
    /// and every cross-subgraph key hop.
    pub(super) fn fragment_options(
        &self,
        pending: &PendingSelection,
        fragment_selection: &InlineFragmentSelection,
    ) -> Result<Vec<RoutingChoice>, FederationError> {
        let mut options = Vec::new();

        if let Some(edge_idx) = self.edge_for_inline_fragment(
            pending.query_graph_node,
            &fragment_selection.inline_fragment,
        ) {
            let (_, target) = self.query_graph.edge_endpoints(edge_idx)?;
            let target_node = self.query_graph.node_weight(target)?;
            options.push(RoutingChoice::direct(edge_idx, target_node.source.clone()));
        }

        let Some(type_cond) = &fragment_selection.inline_fragment.type_condition_position else {
            return Ok(options);
        };

        // @interfaceObject fake downcast: the concrete type doesn't exist in
        // this subgraph.
        for edge_idx in self.out_edge_indices(pending.query_graph_node) {
            let edge_weight = self.query_graph.edge_weight(edge_idx)?;
            if let QueryGraphEdgeTransition::InterfaceObjectFakeDownCast { to_type_name, .. } =
                &edge_weight.transition
                && type_cond.type_name() == to_type_name
            {
                let (_, target) = self.query_graph.edge_endpoints(edge_idx)?;
                let target_node = self.query_graph.node_weight(target)?;
                let has_local_sub_sel =
                    fragment_selection
                        .selection_set
                        .selections
                        .values()
                        .any(|sel| match sel {
                            Selection::Field(f) if *f.field.name() != TYPENAME_FIELD => {
                                self.edge_for_field(target, &f.field).is_some()
                            }
                            _ => false,
                        });
                if has_local_sub_sel {
                    options.push(RoutingChoice::direct(edge_idx, target_node.source.clone()));
                }
                break;
            }
        }

        trace!(
            type_condition = %type_cond.type_name(),
            "searching key hops for fragment downcast",
        );
        let hops = self.key_hop_options(pending.query_graph_node, |key_target| {
            self.edge_for_inline_fragment(key_target, &fragment_selection.inline_fragment)
        })?;
        options.extend(hops);
        Ok(options)
    }

    /// Order options best-first: @provides beats a direct local edge beats a
    /// key hop whose conditions are locally satisfiable beats a remote hop.
    pub(super) fn rank_options(&self, options: &mut [RoutingChoice]) {
        options.sort_by_key(|opt| {
            let preference = if let Ok(edge) = self.query_graph.edge_weight(opt.edge_index()) {
                match &edge.transition {
                    QueryGraphEdgeTransition::FieldCollection {
                        is_part_of_provides: true,
                        ..
                    } => RoutingPreference::Provides,
                    _ if opt.hop_kind == HopKind::Direct => RoutingPreference::DirectLocal,
                    _ if !opt.intermediate_key_hops.is_empty() => RoutingPreference::ChainedKeyHop,
                    _ if opt.conditions_locally_satisfiable => {
                        RoutingPreference::LocallySatisfiableKeyHop
                    }
                    _ => RoutingPreference::RemoteKeyHop,
                }
            } else {
                RoutingPreference::RemoteKeyHop
            };
            let key_size = opt
                .key_conditions
                .as_ref()
                .map(|conds| selection_leaf_count(conds))
                .unwrap_or(0);
            (preference, key_size)
        });
    }

    /// Count immediate sub-selections with a FieldCollection edge at the
    /// given node; used to rank root options.
    fn count_local_sub_selections(&self, target_node: NodeIndex, sub_ss: &SelectionSet) -> usize {
        let mut count = 0;
        for sel in sub_ss.selections.values() {
            if let Selection::Field(f) = sel {
                if *f.field.name() == TYPENAME_FIELD {
                    continue;
                }
                if self.edge_for_field(target_node, &f.field).is_some() {
                    count += 1;
                }
            }
        }
        count
    }

    /// True when the node has no reachable cross-subgraph edges: every
    /// descendant field is local, so the entire subtree can be added in one
    /// shot instead of field-by-field.
    pub(super) fn is_fully_local(
        &self,
        query_graph_node: NodeIndex,
    ) -> Result<bool, FederationError> {
        let node = self.query_graph.node_weight(query_graph_node)?;
        Ok(!node.has_reachable_cross_subgraph_edges)
    }

    /// Recursively check that every sub-selection has an edge at the given
    /// node.
    pub(super) fn all_sub_selections_available(
        &self,
        node: NodeIndex,
        selections: &SelectionSet,
    ) -> Result<bool, FederationError> {
        for sel in selections.selections.values() {
            match sel {
                Selection::Field(field_sel) => {
                    if *field_sel.field.name() == TYPENAME_FIELD {
                        continue;
                    }
                    match self.edge_for_field(node, &field_sel.field) {
                        None => return Ok(false),
                        Some(edge_idx) => {
                            if let Some(sub_ss) = field_sel.selection_set.as_ref() {
                                let target = self
                                    .query_graph
                                    .graph()
                                    .edge_endpoints(edge_idx)
                                    .ok_or_else(|| {
                                        FederationError::internal("edge missing endpoints")
                                    })?
                                    .1;
                                if !self.all_sub_selections_available(target, sub_ss)? {
                                    return Ok(false);
                                }
                            }
                        }
                    }
                }
                Selection::InlineFragment(frag_sel) => {
                    match self.edge_for_inline_fragment(node, &frag_sel.inline_fragment) {
                        None => return Ok(false),
                        Some(edge_idx) => {
                            let target = self
                                .query_graph
                                .graph()
                                .edge_endpoints(edge_idx)
                                .ok_or_else(|| FederationError::internal("edge missing endpoints"))?
                                .1;
                            if !self
                                .all_sub_selections_available(target, &frag_sel.selection_set)?
                            {
                                return Ok(false);
                            }
                        }
                    }
                }
            }
        }
        Ok(true)
    }
}

/// Recursive count of leaf field selections; used to compare @key condition
/// sizes when ranking key hops.
pub(super) fn selection_leaf_count(selection_set: &SelectionSet) -> usize {
    selection_set
        .selections
        .values()
        .map(|sel| match sel {
            Selection::Field(f) => match f.selection_set.as_ref() {
                Some(sub) if !sub.is_empty() => selection_leaf_count(sub),
                _ => 1,
            },
            Selection::InlineFragment(f) => selection_leaf_count(&f.selection_set),
        })
        .sum()
}
