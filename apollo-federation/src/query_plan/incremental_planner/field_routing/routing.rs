use std::sync::Arc;

use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;
use tracing::trace;

use super::FieldRoutingSearchSpace;
use super::RoutingSiteKey;
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
    /// Per-concrete-type explosion at an abstract position: field on an
    /// abstract type with no direct FieldCollection edge decomposes into
    /// per-concrete-type inline fragments. Only ever a forced fallback.
    TypeExplosion,
    /// Restructure an inline fragment with no routing options instead of
    /// dropping it: commit runs the pass-through / vacuous-condition /
    /// abstract-explosion chain, pushing replacement pendings and adding no
    /// graph state. Only ever offered as the single (forced) choice when
    /// normal enumeration yields nothing, so it never competes for score.
    RestructureFragment,
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
    /// The key conditions are @external per the subgraph schema, but an
    /// ancestor's @provides makes them available at this position: every
    /// condition field has a real query-graph edge either at the pending's
    /// node (a provides-copy node) or at the provides-copy anchor the
    /// position descended from via downcasts
    /// ([`PendingSelection::provides_anchor`]). When true, commit appends
    /// the conditions to the parent fetch (the subgraph echoes provided
    /// fields) instead of pushing them as pendings. Exact, path-level
    /// provenance — never a type-level guess.
    pub(crate) conditions_provided: bool,
    /// When the routed edge carries @requires conditions: whether the fetch
    /// anchoring the field can select the condition fields in place. Commit
    /// applies this verdict instead of re-deriving it. True when the edge
    /// has no conditions.
    pub(crate) requires_resolvable_in_place: bool,
    /// A key hop back into the current subgraph (entity re-entry), created
    /// so @requires conditions the fetch cannot select in place ride the
    /// entity representation. Ranked above cross-subgraph hops.
    pub(crate) self_entity_reentry: bool,
    /// The key conditions for this hop are not routable as ordinary
    /// pendings (e.g. circular keys, missing subgraph edges). Commit
    /// handles these via `commit_circular_key_conditions`, selecting the
    /// locally satisfiable subset and failing if it doesn't cover the key.
    pub(crate) conditions_unroutable: bool,
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
    /// Non-edge fallback choice (type explosion or fragment restructuring).
    fn fallback(target: RoutingTarget) -> Self {
        debug_assert!(
            matches!(
                target,
                RoutingTarget::TypeExplosion | RoutingTarget::RestructureFragment
            ),
            "fallback() is only for non-edge routing targets"
        );
        Self {
            target,
            hop_kind: HopKind::Direct,
            key_conditions: None,
            conditions_locally_satisfiable: true,
            conditions_provided: false,
            requires_resolvable_in_place: true,
            self_entity_reentry: false,
            conditions_unroutable: false,
            intermediate_key_hops: Vec::new(),
        }
    }

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
            conditions_provided: false,
            requires_resolvable_in_place: true,
            self_entity_reentry: false,
            conditions_unroutable: false,
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
            conditions_provided: false,
            requires_resolvable_in_place,
            self_entity_reentry: true,
            conditions_unroutable: false,
            intermediate_key_hops: Vec::new(),
        }
    }

    /// Target subgraph name.
    pub(crate) fn target_subgraph(&self) -> &Arc<str> {
        match &self.target {
            RoutingTarget::SubgraphEdge {
                target_subgraph, ..
            } => target_subgraph,
            RoutingTarget::TypeExplosion => {
                static LABEL: std::sync::LazyLock<Arc<str>> =
                    std::sync::LazyLock::new(|| Arc::from("<type-explosion>"));
                &LABEL
            }
            RoutingTarget::RestructureFragment => {
                static LABEL: std::sync::LazyLock<Arc<str>> =
                    std::sync::LazyLock::new(|| Arc::from("<restructure>"));
                &LABEL
            }
        }
    }

    /// Query graph edge index. Panics on non-edge routing choices.
    pub(crate) fn edge_index(&self) -> EdgeIndex {
        match &self.target {
            RoutingTarget::SubgraphEdge { edge_index, .. } => *edge_index,
            RoutingTarget::TypeExplosion | RoutingTarget::RestructureFragment => {
                panic!("edge_index() called on a non-edge routing choice")
            }
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
    /// A hop with statically circular key conditions: viable only when
    /// the anchor already provides the full key, so every other hop is
    /// preferred.
    CircularKeyHop,
    /// Restructure an inline fragment when normal enumeration yields nothing.
    RestructureFragment,
    /// Per-concrete-type explosion at an abstract position. Defers real
    /// fetch cost to child fragments, so always ranked last.
    TypeExplosion,
}

struct KeyHopCandidate {
    found_edge_idx: EdgeIndex,
    target_subgraph: Arc<str>,
    is_root: bool,
    conditions_local: bool,
    conditions_provided: bool,
    key_conditions: Option<Arc<SelectionSet>>,
    key_leaf_count: usize,
}

impl KeyHopCandidate {
    /// Locally satisfiable (or @provides-covered) keys must never lose the
    /// per-subgraph dedup to a cheaper-looking unsatisfiable key.
    fn dedup_rank(&self) -> (u8, usize) {
        let satisfiability = if self.conditions_local {
            0
        } else if self.conditions_provided {
            1
        } else {
            2
        };
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
        let edge = self.qg().edge_weight(edge_idx)?;
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
        force_hop: bool,
    ) -> Result<(), FederationError> {
        let in_place = self.requires_conditions_resolvable_in_place(node, edge_idx)?;
        let key = self
            .cached_query_graph
            .query_graph
            .get_locally_satisfiable_key(node)?;
        if in_place && !(force_hop && key.is_some()) {
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
        provides_anchor: Option<NodeIndex>,
        edge_finder: impl Fn(NodeIndex) -> Option<EdgeIndex>,
    ) -> Result<Vec<RoutingChoice>, FederationError> {
        let current_node = self
            .cached_query_graph
            .query_graph
            .node_weight(pending_node)?;
        let current_source = current_node.source.clone();
        let source_type: Option<CompositeTypeDefinitionPosition> =
            current_node.type_.clone().try_into().ok();
        let source_schema = self
            .cached_query_graph
            .query_graph
            .schema_by_source(&current_source)
            .ok();

        let mut candidates: Vec<KeyHopCandidate> = Vec::new();
        let mut need_chain: Vec<(NodeIndex, EdgeIndex)> = Vec::new();

        for key_edge_idx in self
            .cached_query_graph
            .out_edges(pending_node)
            .iter()
            .copied()
        {
            let key_edge = self
                .cached_query_graph
                .query_graph
                .edge_weight(key_edge_idx)?;
            if !matches!(
                key_edge.transition,
                QueryGraphEdgeTransition::KeyResolution
                    | QueryGraphEdgeTransition::RootTypeResolution { .. }
            ) {
                continue;
            }
            let (_, key_target) = self
                .cached_query_graph
                .query_graph
                .edge_endpoints(key_edge_idx)?;
            let key_target_node = self
                .cached_query_graph
                .query_graph
                .node_weight(key_target)?;
            if key_target_node.source == current_source {
                continue;
            }
            if let Some(found_edge_idx) = edge_finder(key_target) {
                let candidate = self.single_hop_candidate(
                    pending_node,
                    found_edge_idx,
                    key_edge,
                    key_target_node.source.clone(),
                    provides_anchor,
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
        // reached the field; chained hops are strictly more expensive
        // (two or more entity fetches) and are dominated by any direct hop.
        // A hop with circular key conditions doesn't count as reaching: its
        // commit fails unless the anchor resolves the whole key, so a
        // satisfiable chain (e.g. through a subgraph keyed on a field the
        // state does have) must still be offered; ranking already prefers
        // chains over circular hops.
        if options.iter().all(|opt| opt.conditions_unroutable) {
            let single_hop_count = options.len();
            for (key_target, key_edge_idx) in need_chain {
                let key_edge = self
                    .cached_query_graph
                    .query_graph
                    .edge_weight(key_edge_idx)?;
                for option in self.chained_key_hop_options(
                    pending_node,
                    key_target,
                    key_edge,
                    &current_source,
                    &source_type,
                    &source_schema,
                    provides_anchor,
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
        provides_anchor: Option<NodeIndex>,
        edge_finder: &impl Fn(NodeIndex) -> Option<EdgeIndex>,
    ) -> Result<Vec<RoutingChoice>, FederationError> {
        let first_conditions_local = match (&first_key_edge.conditions, origin_type, origin_schema)
        {
            (Some(conds), Some(st), Some(ss)) => self.can_satisfy(conds, st, ss),
            (None, _, _) => true,
            _ => false,
        };
        let mut first_conditions_provided = false;
        let mut first_conditions_unroutable = false;
        if !first_conditions_local && let Some(conds) = &first_key_edge.conditions {
            first_conditions_provided =
                self.key_conditions_provided(origin_node, provides_anchor, conds)?;
        }
        if !first_conditions_local
            && !first_conditions_provided
            && let Some(conds) = &first_key_edge.conditions
        {
            first_conditions_unroutable = !self.conditions_routable(origin_node, conds)?;
        }

        let first_intermediate_data = self
            .cached_query_graph
            .query_graph
            .node_weight(first_intermediate)?;
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

            for key_edge_idx in self.cached_query_graph.out_edges(current).iter().copied() {
                let key_edge = self
                    .cached_query_graph
                    .query_graph
                    .edge_weight(key_edge_idx)?;
                if !matches!(key_edge.transition, QueryGraphEdgeTransition::KeyResolution) {
                    continue;
                }
                let (_, next_target) = self
                    .cached_query_graph
                    .query_graph
                    .edge_endpoints(key_edge_idx)?;
                let next_target_data = self
                    .cached_query_graph
                    .query_graph
                    .node_weight(next_target)?;
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
                        conditions_provided: first_conditions_provided,
                        requires_resolvable_in_place: self
                            .requires_conditions_resolvable_in_place(origin_node, found_edge_idx)?,
                        self_entity_reentry: false,
                        conditions_unroutable: first_conditions_unroutable,
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
    #[allow(clippy::too_many_arguments)]
    fn single_hop_candidate(
        &self,
        pending_node: NodeIndex,
        found_edge_idx: EdgeIndex,
        key_edge: &crate::query_graph::QueryGraphEdge,
        target_subgraph: Arc<str>,
        provides_anchor: Option<NodeIndex>,
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
                    self.can_satisfy(conds, st, ss)
                }
                (None, _, _) => true,
                _ => false,
            }
        };
        let conditions_provided = if !conditions_local && !is_root {
            match &key_edge.conditions {
                Some(conds) => {
                    self.key_conditions_provided(pending_node, provides_anchor, conds)?
                }
                None => false,
            }
        } else {
            false
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
            conditions_provided,
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
            let conditions_provided = c.conditions_provided;
            let mut conditions_unroutable = false;
            if !c.is_root
                && !c.conditions_local
                && !conditions_provided
                && let Some(conds) = &c.key_conditions
            {
                conditions_unroutable = !self.conditions_routable(pending_node, conds)?;
            }
            trace!(
                target_subgraph = %c.target_subgraph,
                conditions_local = c.conditions_local,
                conditions_provided,
                conditions_unroutable,
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
                conditions_provided,
                requires_resolvable_in_place: self
                    .requires_conditions_resolvable_in_place(pending_node, c.found_edge_idx)?,
                self_entity_reentry: false,
                conditions_unroutable,
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
        let current_node_data = self
            .cached_query_graph
            .query_graph
            .node_weight(pending.query_graph_node)?;
        let mut options = if matches!(
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
        if !self.disabled_subgraphs.is_empty() {
            options.retain(|opt| !self.disabled_subgraphs.contains(opt.target_subgraph()));
        }
        // Zero options is not failure yet: the applicable fallback
        // (fragment restructuring, or type explosion at an abstract field
        // position) becomes the single forced choice, committed through the
        // same backbone as every other option.
        if options.is_empty()
            && let Some(fallback) = self.fallback_option(pending)?
        {
            options.push(fallback);
        }
        Ok(options)
    }

    /// Cached wrapper around `routing_options`: keyed by (node, selection
    /// pointer, override conditions pointer) so repeated evaluations of the
    /// same pending at the same position return the prior result immediately.
    pub(super) fn cached_routing_options(
        &self,
        pending: &PendingSelection,
    ) -> Result<Arc<Vec<RoutingChoice>>, FederationError> {
        let key = (
            pending.query_graph_node,
            super::SelectionArcKey::new(&pending.selection),
            None::<super::ArcKey<std::collections::HashSet<apollo_compiler::Name>>>,
        );
        let unfiltered = if let Some(cached) = self.caches.routing_options.borrow().get(&key) {
            cached.clone()
        } else {
            let result = Arc::new(self.routing_options(pending)?);
            self.caches
                .routing_options
                .borrow_mut()
                .insert(key, result.clone());
            result
        };
        if let Some(avoid) = &pending.split_avoid {
            let options = unfiltered
                .iter()
                .filter(|choice| choice.target_subgraph() != avoid)
                .cloned()
                .collect::<Vec<_>>();
            return Ok(Arc::new(options));
        }
        Ok(unfiltered)
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
        for entry_edge_idx in self
            .cached_query_graph
            .out_edges(pending.query_graph_node)
            .iter()
            .copied()
        {
            let entry_edge = self
                .cached_query_graph
                .query_graph
                .edge_weight(entry_edge_idx)?;
            if !matches!(
                entry_edge.transition,
                QueryGraphEdgeTransition::SubgraphEnteringTransition
            ) {
                continue;
            }
            let (_, subgraph_root) = self
                .cached_query_graph
                .query_graph
                .edge_endpoints(entry_edge_idx)?;
            if let Some(field_edge_idx) = self
                .cached_query_graph
                .edge_for_field(subgraph_root, &field_selection.field)
            {
                let subgraph_node = self
                    .cached_query_graph
                    .query_graph
                    .node_weight(subgraph_root)?;
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
                    .cached_query_graph
                    .query_graph
                    .edge_endpoints(a.edge_index())
                    .ok()
                    .map(|(_, target)| self.count_local_sub_selections(target, sub_ss))
                    .unwrap_or(0);
                let count_b = self
                    .cached_query_graph
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
    /// later. Ranking keeps the direct edge first, so a greedy pass that later
    /// strands a descendant relies on BULB backtracking to revisit the hop.
    ///
    /// At an abstract-type position whose concrete implementers have
    /// cross-subgraph entity keys, per-concrete-type explosion is a
    /// genuine alternative: a direct interface-level edge whose target strands
    /// a descendant is otherwise a forced commit with no decision point to
    /// backtrack into. Enumerating it (ranked last via Fallback preference)
    /// makes the escape a normal search decision.
    pub(super) fn field_options(
        &self,
        pending: &PendingSelection,
        field_selection: &FieldSelection,
    ) -> Result<Vec<RoutingChoice>, FederationError> {
        let mut options = Vec::new();
        if let Some(edge_idx) = self
            .cached_query_graph
            .edge_for_field(pending.query_graph_node, &field_selection.field)
        {
            let (_, target) = self
                .cached_query_graph
                .query_graph
                .edge_endpoints(edge_idx)?;
            let target_node = self.qg().node_weight(target)?;
            let edge = self.qg().edge_weight(edge_idx)?;
            // @fromContext at a position the entity boundary does not already
            // isolate needs a same-subgraph entity re-entry so the context
            // value rides the representation.
            let needs_isolation = !edge.required_contexts.is_empty()
                && super::context::needs_context_isolation(pending, &edge.required_contexts);
            if edge.conditions.is_none() && !needs_isolation {
                options.push(RoutingChoice::direct(edge_idx, target_node.source.clone()));
            } else {
                self.push_requires_strategy_options(
                    &mut options,
                    pending.query_graph_node,
                    edge_idx,
                    &target_node.source,
                    needs_isolation,
                )?;
            }
        }

        let key = RoutingSiteKey::Field(field_selection.field.name().clone());
        let hops = self.cached_key_hops(
            pending.query_graph_node,
            pending.provides_anchor,
            key,
            |key_target| {
                self.cached_query_graph
                    .edge_for_field(key_target, &field_selection.field)
            },
        )?;
        options.extend(hops.iter().cloned());

        if !options.is_empty()
            && self.abstract_type_has_cross_subgraph_keys(pending.query_graph_node)?
        {
            options.push(RoutingChoice::fallback(RoutingTarget::TypeExplosion));
        }

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

        if let Some(edge_idx) = self.cached_query_graph.edge_for_inline_fragment(
            pending.query_graph_node,
            &fragment_selection.inline_fragment,
        ) {
            let (_, target) = self
                .cached_query_graph
                .query_graph
                .edge_endpoints(edge_idx)?;
            let target_node = self.qg().node_weight(target)?;
            options.push(RoutingChoice::direct(edge_idx, target_node.source.clone()));
        }

        let Some(type_cond) = &fragment_selection.inline_fragment.type_condition_position else {
            return Ok(options);
        };

        // @interfaceObject fake downcast: the concrete type doesn't exist in
        // this subgraph.
        for edge_idx in self
            .cached_query_graph
            .out_edges(pending.query_graph_node)
            .iter()
            .copied()
        {
            let edge_weight = self.qg().edge_weight(edge_idx)?;
            if let QueryGraphEdgeTransition::InterfaceObjectFakeDownCast { to_type_name, .. } =
                &edge_weight.transition
                && type_cond.type_name() == to_type_name
            {
                let (_, target) = self
                    .cached_query_graph
                    .query_graph
                    .edge_endpoints(edge_idx)?;
                let target_node = self.qg().node_weight(target)?;
                let has_local_sub_sel =
                    fragment_selection
                        .selection_set
                        .selections
                        .values()
                        .any(|sel| match sel {
                            Selection::Field(f) if *f.field.name() != TYPENAME_FIELD => self
                                .cached_query_graph
                                .edge_for_field(target, &f.field)
                                .is_some(),
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
        let key = RoutingSiteKey::InlineFragment(Some(type_cond.type_name().clone()));
        let hops = self.cached_key_hops(
            pending.query_graph_node,
            pending.provides_anchor,
            key,
            |key_target| {
                self.cached_query_graph
                    .edge_for_inline_fragment(key_target, &fragment_selection.inline_fragment)
            },
        )?;
        options.extend(hops.iter().cloned());
        Ok(options)
    }

    /// Fallback when normal edge enumeration yields no options: fragment
    /// restructuring for inline fragments, type explosion for fields on
    /// abstract types.
    fn fallback_option(
        &self,
        pending: &PendingSelection,
    ) -> Result<Option<RoutingChoice>, FederationError> {
        match &pending.selection {
            Selection::InlineFragment(_) => Ok(Some(RoutingChoice::fallback(
                RoutingTarget::RestructureFragment,
            ))),
            Selection::Field(_) => {
                let node_data =
                    self.qg().node_weight(pending.query_graph_node)?;
                let is_abstract = matches!(
                    CompositeTypeDefinitionPosition::try_from(node_data.type_.clone()),
                    Ok(pos) if pos.is_abstract_type()
                );
                Ok(is_abstract.then(|| RoutingChoice::fallback(RoutingTarget::TypeExplosion)))
            }
        }
    }

    /// Whether any concrete implementer of the abstract type at `node` has a
    /// cross-subgraph key edge. Type explosion is only useful when at least
    /// one implementer can be routed to a different subgraph via an entity
    /// key, so this gates the TypeExplosion fallback to avoid doubling the
    /// BULB search tree at every abstract-type field.
    fn abstract_type_has_cross_subgraph_keys(
        &self,
        node: NodeIndex,
    ) -> Result<bool, FederationError> {
        let qg = self.qg();
        let node_data = qg.node_weight(node)?;
        let current_source = &node_data.source;
        let Ok(pos) = CompositeTypeDefinitionPosition::try_from(node_data.type_.clone()) else {
            return Ok(false);
        };
        if !pos.is_abstract_type() {
            return Ok(false);
        }
        let schema = qg.schema_by_source(current_source)?;
        let runtime_types = schema.possible_runtime_types(pos)?;
        for concrete_type in &runtime_types {
            let type_name = &concrete_type.type_name;
            let Ok(nodes) = qg.nodes_for_type(type_name) else {
                continue;
            };
            for &concrete_node in nodes {
                let concrete_data = qg.node_weight(concrete_node)?;
                if &concrete_data.source != current_source {
                    continue;
                }
                for edge_idx in self.cached_query_graph.out_edges(concrete_node).iter().copied() {
                    let edge = qg.edge_weight(edge_idx)?;
                    if matches!(edge.transition, QueryGraphEdgeTransition::KeyResolution) {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// Order options best-first: @provides beats a direct local edge beats a
    /// key hop whose conditions are locally satisfiable beats a remote hop.
    pub(super) fn rank_options(&self, options: &mut [RoutingChoice]) {
        options.sort_by_key(|opt| {
            let edge_index = match &opt.target {
                RoutingTarget::SubgraphEdge { edge_index, .. } => *edge_index,
                RoutingTarget::TypeExplosion => {
                    return (RoutingPreference::TypeExplosion, 0)
                }
                RoutingTarget::RestructureFragment => {
                    return (RoutingPreference::RestructureFragment, 0)
                }
            };
            let preference = if let Ok(edge) =
                self.qg().edge_weight(edge_index)
            {
                match &edge.transition {
                    QueryGraphEdgeTransition::FieldCollection {
                        is_part_of_provides: true,
                        ..
                    } => RoutingPreference::Provides,
                    _ if opt.hop_kind == HopKind::Direct => RoutingPreference::DirectLocal,
                    _ if opt.self_entity_reentry => RoutingPreference::SelfRequiresHop,
                    _ if opt.conditions_unroutable => RoutingPreference::CircularKeyHop,
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

    /// Wrap `key_hop_options` with the in-flight cycle guard. All call
    /// sites that enumerate key hops must go through this wrapper so the
    /// mutual recursion between hop enumeration and `conditions_routable`
    /// terminates: re-entering a (node, key) already on the stack means
    /// the key conditions require the field being hopped for, creating a circular
    /// key whose fixpoint is "no hops".
    fn key_hops_guarded(
        &self,
        node: NodeIndex,
        provides_anchor: Option<NodeIndex>,
        key: RoutingSiteKey,
        edge_finder: impl Fn(NodeIndex) -> Option<EdgeIndex>,
    ) -> Result<Vec<RoutingChoice>, FederationError> {
        if !self
            .caches
            .key_hops_in_flight
            .borrow_mut()
            .insert((node, key.clone()))
        {
            trace!(
                ?node,
                ?key,
                "key-hop cycle guard hit: circular key resolves to no hops"
            );
            self.caches.guard_hits.set(self.caches.guard_hits.get() + 1);
            return Ok(Vec::new());
        }
        let result = self.key_hop_options(node, provides_anchor, edge_finder);
        self.caches
            .key_hops_in_flight
            .borrow_mut()
            .remove(&(node, key));
        result
    }

    /// Cached wrapper around `key_hops_guarded`: if the guard fires (cycle),
    /// we record that as a guard_hit and return empty without caching, so
    /// the outermost call still gets the real result and caches it.
    pub(super) fn cached_key_hops(
        &self,
        node: NodeIndex,
        provides_anchor: Option<NodeIndex>,
        key: RoutingSiteKey,
        edge_finder: impl Fn(NodeIndex) -> Option<EdgeIndex>,
    ) -> Result<Arc<Vec<RoutingChoice>>, FederationError> {
        let cache_key = (node, key.clone());
        if let Some(cached) = self.caches.key_hops.borrow().get(&cache_key) {
            return Ok(cached.clone());
        }
        let guard_before = self.caches.guard_hits.get();
        let result = self.key_hops_guarded(node, provides_anchor, key, edge_finder)?;
        let guard_after = self.caches.guard_hits.get();
        let result = Arc::new(result);
        // Only cache when the guard didn't fire during this call: a guard
        // hit means the result was truncated by the cycle breaker, so it's
        // correct only for this particular call-stack depth.
        if guard_before == guard_after {
            self.caches
                .key_hops
                .borrow_mut()
                .insert(cache_key, result.clone());
        }
        Ok(result)
    }

    /// Whether key conditions that the schema calls @external are still
    /// graph-resolvable at the pending's position thanks to an ancestor
    /// @provides: an external key field covered by an ancestor @provides
    /// has real edges at the provides-copy node (or at the anchor).
    fn key_conditions_provided(
        &self,
        node: NodeIndex,
        provides_anchor: Option<NodeIndex>,
        conditions: &SelectionSet,
    ) -> Result<bool, FederationError> {
        if self.conditions_resolvable_at_node(node, conditions)? {
            return Ok(true);
        }
        match provides_anchor {
            Some(anchor) => self.conditions_resolvable_at_node(anchor, conditions),
            None => Ok(false),
        }
    }

    /// Whether not-locally-satisfiable key conditions can actually be
    /// fetched from `node`. Each condition field must have some viable
    /// route: a direct edge, or a key hop that can reach it. The mutual
    /// recursion (hop viability depends on condition routability, which
    /// depends on hop viability) is broken by the in-flight guard in
    /// `key_hops_guarded`.
    fn conditions_routable(
        &self,
        node: NodeIndex,
        conditions: &SelectionSet,
    ) -> Result<bool, FederationError> {
        let cache_key = (node, super::ArcKey::new(&conditions.selections));
        if let Some(&cached) = self.caches.conditions_routable.borrow().get(&cache_key) {
            return Ok(cached);
        }
        let guard_before = self.caches.guard_hits.get();
        let mut result = true;
        for sel in conditions.selections.values() {
            let routable = match sel {
                Selection::Field(field_sel) => self.condition_field_routable(node, field_sel)?,
                Selection::InlineFragment(frag_sel) => {
                    self.condition_fragment_routable(node, frag_sel)?
                }
            };
            if !routable {
                result = false;
                break;
            }
        }
        let guard_after = self.caches.guard_hits.get();
        if guard_before == guard_after {
            self.caches
                .conditions_routable
                .borrow_mut()
                .insert(cache_key, result);
        }
        Ok(result)
    }

    fn condition_field_routable(
        &self,
        node: NodeIndex,
        field_sel: &FieldSelection,
    ) -> Result<bool, FederationError> {
        if *field_sel.field.name() == TYPENAME_FIELD {
            return Ok(true);
        }
        let sub_ss = field_sel.selection_set.as_ref().filter(|s| !s.is_empty());
        if let Some(edge_idx) = self
            .cached_query_graph
            .edge_for_field(node, &field_sel.field)
        {
            let Some(sub_ss) = sub_ss else {
                return Ok(true);
            };
            let (_, target) = self
                .cached_query_graph
                .query_graph
                .edge_endpoints(edge_idx)?;
            if self.conditions_routable(target, sub_ss)? {
                return Ok(true);
            }
            // Direct edge exists but its subtree dead-ends; a key hop at
            // this level may still reach it.
        }
        // No provides anchor: only hop existence matters here, which the
        // anchor never changes (it only refines `conditions_provided`).
        let key = RoutingSiteKey::Field(field_sel.field.name().clone());
        let hops = self.cached_key_hops(node, None, key, |key_target| {
            self.cached_query_graph
                .edge_for_field(key_target, &field_sel.field)
        })?;
        self.hops_reach(&hops, sub_ss)
    }

    fn condition_fragment_routable(
        &self,
        node: NodeIndex,
        frag_sel: &InlineFragmentSelection,
    ) -> Result<bool, FederationError> {
        let Some(type_cond) = frag_sel.inline_fragment.type_condition_position.as_ref() else {
            return self.conditions_routable(node, &frag_sel.selection_set);
        };
        if let Some(edge_idx) = self
            .cached_query_graph
            .edge_for_inline_fragment(node, &frag_sel.inline_fragment)
        {
            let (_, target) = self
                .cached_query_graph
                .query_graph
                .edge_endpoints(edge_idx)?;
            return self.conditions_routable(target, &frag_sel.selection_set);
        }
        // No provides anchor: only which subgraphs hops reach matters here,
        // not `conditions_provided`.
        let key = RoutingSiteKey::InlineFragment(Some(type_cond.type_name().clone()));
        let hops = self.cached_key_hops(node, None, key, |key_target| {
            self.cached_query_graph
                .edge_for_inline_fragment(key_target, &frag_sel.inline_fragment)
        })?;
        self.hops_reach(&hops, Some(&frag_sel.selection_set))
    }

    /// Whether any of `hops` can deliver `sub_ss`: at least one hop whose
    /// target routes the sub-selections.
    fn hops_reach(
        &self,
        hops: &[RoutingChoice],
        sub_ss: Option<&SelectionSet>,
    ) -> Result<bool, FederationError> {
        if hops.is_empty() {
            return Ok(false);
        }
        let Some(sub_ss) = sub_ss else {
            return Ok(true);
        };
        for hop in hops {
            let (_, target) = self
                .cached_query_graph
                .query_graph
                .edge_endpoints(hop.edge_index())?;
            if self.conditions_routable(target, sub_ss)? {
                return Ok(true);
            }
        }
        Ok(false)
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
                if self
                    .cached_query_graph
                    .edge_for_field(target_node, &f.field)
                    .is_some()
                {
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
        let node = self
            .cached_query_graph
            .query_graph
            .node_weight(query_graph_node)?;
        Ok(!node.has_reachable_cross_subgraph_edges)
    }

    /// Recursively check that every sub-selection has an edge at the given
    /// node and that no edge carries @fromContext conditions (which need
    /// special plumbing the bulk-insert path skips).
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
                    match self
                        .cached_query_graph
                        .edge_for_field(node, &field_sel.field)
                    {
                        None => return Ok(false),
                        Some(edge_idx) => {
                            let edge = self.cached_query_graph.query_graph.edge_weight(edge_idx)?;
                            if !edge.required_contexts.is_empty() {
                                return Ok(false);
                            }
                            if let Some(sub_ss) = field_sel.selection_set.as_ref() {
                                let target = self
                                    .cached_query_graph
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
                    match self
                        .cached_query_graph
                        .edge_for_inline_fragment(node, &frag_sel.inline_fragment)
                    {
                        None => return Ok(false),
                        Some(edge_idx) => {
                            let target = self
                                .cached_query_graph
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
