use std::collections::VecDeque;
use std::sync::Arc;

use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef as _;
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

/// Longest key-hop chain the BFS explores before pruning. Five hops
/// covers realistic federation graphs, where entities rarely need more
/// than two or three intermediate key resolutions to reach a target
/// subgraph.
const MAX_CHAIN_DEPTH: usize = 5;

/// Common fields for every edge-based routing choice.
#[derive(Clone, Debug)]
pub(crate) struct EdgeInfo {
    pub(crate) edge_index: EdgeIndex,
    pub(crate) target_subgraph: Arc<str>,
}

/// Key-hop metadata carried by key-based routing choices.
#[derive(Clone, Debug)]
pub(crate) struct KeyHopInfo {
    /// @key fields entering the target group.
    pub(crate) key_conditions: Arc<SelectionSet>,
    /// Whether the anchor fetch can select @requires conditions in place.
    pub(crate) requires_resolvable_in_place: bool,
    /// The key conditions for this hop are not routable as ordinary
    /// pendings (e.g. circular keys, missing subgraph edges). Commit
    /// handles these specially via `commit_circular_key_conditions`,
    /// selecting the locally satisfiable subset and failing if it
    /// doesn't cover the key.
    pub(crate) conditions_unroutable: bool,
}

/// One leg of a multi-hop key chain: an intermediate node and the key
/// that enters it. Every key in a choice is an entry key; the previous
/// group outputs whatever enters the next.
#[derive(Clone, Debug)]
pub(crate) struct IntermediateKeyHop {
    pub(crate) target_node: NodeIndex,
    pub(crate) entry_key: Option<Arc<SelectionSet>>,
}

/// A routing decision for a pending selection. Variant declaration
/// order defines the sort ranking (best first).
#[derive(Clone, Debug)]
pub(crate) enum RoutingChoice {
    /// Direct edge created by @provides.
    Provides(EdgeInfo),
    /// Direct edge in the current subgraph.
    Local(EdgeInfo),
    /// Key hop whose @key conditions are locally available.
    KeyHopWithLocalKey { edge: EdgeInfo, key: KeyHopInfo },
    /// Key hop whose @key conditions are covered by an ancestor @provides.
    #[allow(dead_code)]
    KeyHopWithProvidedKey { edge: EdgeInfo, key: KeyHopInfo },
    /// Key hop whose @key conditions must be fetched from other subgraphs.
    KeyHopWithExternalKey { edge: EdgeInfo, key: KeyHopInfo },
    /// Hop to the root type of another subgraph.
    RootHop(EdgeInfo),
    /// Multi-hop key chain through intermediate subgraphs.
    ChainedKeyHop {
        edge: EdgeInfo,
        key: KeyHopInfo,
        intermediate_hops: Vec<IntermediateKeyHop>,
    },
    /// Key hop with statically circular conditions.
    CircularKeyHop {
        edge: EdgeInfo,
        key: KeyHopInfo,
        intermediate_hops: Vec<IntermediateKeyHop>,
    },
    /// Strip a fragment which provides no routing information.
    StripFragment,
    /// Per-concrete-type explosion at an abstract position.
    TypeExplosion,
}

impl RoutingChoice {
    /// Edge info for edge-based choices, `None` for type explosion and
    /// fragment restructuring, which have no associated edge.
    fn edge(&self) -> Option<&EdgeInfo> {
        match self {
            Self::Provides(e)
            | Self::Local(e)
            | Self::RootHop(e)
            | Self::KeyHopWithLocalKey { edge: e, .. }
            | Self::KeyHopWithProvidedKey { edge: e, .. }
            | Self::KeyHopWithExternalKey { edge: e, .. }
            | Self::ChainedKeyHop { edge: e, .. }
            | Self::CircularKeyHop { edge: e, .. } => Some(e),
            Self::StripFragment | Self::TypeExplosion => None,
        }
    }

    /// Key hop metadata, `None` for direct, root, and non-edge choices.
    fn key_opt(&self) -> Option<&KeyHopInfo> {
        match self {
            Self::KeyHopWithLocalKey { key, .. }
            | Self::KeyHopWithProvidedKey { key, .. }
            | Self::KeyHopWithExternalKey { key, .. }
            | Self::ChainedKeyHop { key, .. }
            | Self::CircularKeyHop { key, .. } => Some(key),
            _ => None,
        }
    }

    /// Target subgraph name (synthetic label for non-edge choices).
    pub(crate) fn target_subgraph(&self) -> &Arc<str> {
        match self.edge() {
            Some(e) => &e.target_subgraph,
            None if matches!(self, Self::TypeExplosion) => {
                static LABEL: std::sync::LazyLock<Arc<str>> =
                    std::sync::LazyLock::new(|| Arc::from("<type-explosion>"));
                &LABEL
            }
            None => {
                static LABEL: std::sync::LazyLock<Arc<str>> =
                    std::sync::LazyLock::new(|| Arc::from("<strip-fragment>"));
                &LABEL
            }
        }
    }

    /// Query graph edge index, if this is an edge-based choice.
    pub(crate) fn edge_index(&self) -> Option<EdgeIndex> {
        self.edge().map(|e| e.edge_index)
    }

    /// Key hop metadata, or an internal error for non-key-hop choices.
    pub(crate) fn key(&self) -> Result<&KeyHopInfo, FederationError> {
        self.key_opt().ok_or_else(|| {
            FederationError::internal("key() called on a non-key-hop routing choice")
        })
    }

    /// Whether this is a direct resolution (no hop needed).
    pub(crate) fn is_direct(&self) -> bool {
        matches!(self, Self::Provides(_) | Self::Local(_))
    }

    /// Whether this is a key hop (entity-based, not root-type-resolution).
    pub(crate) fn is_key_hop(&self) -> bool {
        self.key_opt().is_some()
    }

    /// Intermediate hops for chained key chains, empty otherwise.
    pub(crate) fn intermediate_hops(&self) -> &[IntermediateKeyHop] {
        match self {
            Self::ChainedKeyHop {
                intermediate_hops, ..
            }
            | Self::CircularKeyHop {
                intermediate_hops, ..
            } => intermediate_hops,
            _ => &[],
        }
    }

    /// Whether @requires conditions are resolvable in place. True for
    /// choices with no key hop, which carry no @requires verdict.
    pub(crate) fn requires_resolvable_in_place(&self) -> bool {
        self.key_opt()
            .is_none_or(|key| key.requires_resolvable_in_place)
    }

    /// Whether the key conditions for this hop are unroutable (circular).
    pub(crate) fn conditions_unroutable(&self) -> bool {
        self.key_opt().is_some_and(|key| key.conditions_unroutable)
    }

    /// Planning heuristic for choosing among routing options. Prefers local
    /// selections over key hops, and key hops with locally available data
    /// over ones that require recursive planning through other subgraphs.
    /// Ties are broken by key condition leaf count so smaller keys win.
    pub(super) fn rank(&self) -> (u8, usize) {
        let variant = match self {
            Self::Provides(_) => 0,
            Self::Local(_) => 1,
            Self::KeyHopWithLocalKey { .. } => 2,
            Self::KeyHopWithProvidedKey { .. } => 3,
            Self::KeyHopWithExternalKey { .. } => 4,
            Self::RootHop(_) => 5,
            Self::ChainedKeyHop { .. } => 6,
            Self::CircularKeyHop { .. } => 7,
            Self::StripFragment => 8,
            Self::TypeExplosion => 9,
        };
        let key_size = self
            .key_opt()
            .map_or(0, |key| selection_leaf_count(&key.key_conditions));
        (variant, key_size)
    }
}

/// Per-subgraph dedup candidate for key hops; satisfiable keys must never
/// lose the dedup to a cheaper-looking unsatisfiable one.
struct KeyHopCandidate {
    found_edge_idx: EdgeIndex,
    target_subgraph: Arc<str>,
    is_root: bool,
    conditions_local: bool,
    key_conditions: Option<Arc<SelectionSet>>,
    key_leaf_count: usize,
}

impl KeyHopCandidate {
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
        let edge = self.cached_query_graph.query_graph.edge_weight(edge_idx)?;
        let Some(conditions) = &edge.conditions else {
            return Ok(true);
        };
        let source = self.node_source(node)?;
        self.can_resolve_in_place(node, conditions, &source)
    }

    /// Build a direct routing choice for a field edge, classifying it as
    /// Provides when the edge is part of an @provides subtree.
    fn direct_choice(
        &self,
        edge_idx: EdgeIndex,
        target_subgraph: Arc<str>,
    ) -> Result<RoutingChoice, FederationError> {
        let is_provides = matches!(
            self.cached_query_graph
                .query_graph
                .edge_weight(edge_idx)?
                .transition,
            QueryGraphEdgeTransition::FieldCollection {
                is_part_of_provides: true,
                ..
            }
        );
        let edge_info = EdgeInfo {
            edge_index: edge_idx,
            target_subgraph,
        };
        Ok(if is_provides {
            RoutingChoice::Provides(edge_info)
        } else {
            RoutingChoice::Local(edge_info)
        })
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
            options.push(self.direct_choice(edge_idx, target_subgraph.clone())?);
        }
        if let Some(key) = key {
            options.push(RoutingChoice::KeyHopWithLocalKey {
                edge: EdgeInfo {
                    edge_index: edge_idx,
                    target_subgraph: target_subgraph.clone(),
                },
                key: KeyHopInfo {
                    key_conditions: Arc::new(key),
                    requires_resolvable_in_place: in_place,
                    conditions_unroutable: false,
                },
            });
        }
        Ok(())
    }

    /// Append cross-subgraph key-hop options. Chained hops are a last
    /// resort, explored only when every option so far is circular. The
    /// in-flight guard breaks the mutual recursion with
    /// `conditions_routable`: re-entering a (node, key) already on the
    /// call stack means the key conditions require the selection being
    /// hopped for, so the circular key resolves to no hops.
    pub(super) fn append_key_hop_options(
        &self,
        pending_node: NodeIndex,
        key: RoutingSiteKey,
        options: &mut Vec<RoutingChoice>,
        edge_finder: impl Fn(NodeIndex) -> Option<EdgeIndex>,
    ) -> Result<(), FederationError> {
        if !self
            .caches
            .key_hops_in_flight
            .borrow_mut()
            .insert((pending_node, key.clone()))
        {
            trace!(
                ?pending_node,
                ?key,
                "key-hop cycle guard hit: circular key resolves to no hops"
            );
            return Ok(());
        }
        let result = self.append_key_hop_options_inner(pending_node, options, &edge_finder);
        self.caches
            .key_hops_in_flight
            .borrow_mut()
            .remove(&(pending_node, key));
        result
    }

    fn append_key_hop_options_inner(
        &self,
        pending_node: NodeIndex,
        options: &mut Vec<RoutingChoice>,
        edge_finder: &impl Fn(NodeIndex) -> Option<EdgeIndex>,
    ) -> Result<(), FederationError> {
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
            .query_graph
            .out_edges(pending_node)
            .into_iter()
            .map(|e| e.id())
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
            if key_target_node.source == current_source
                || self.disabled_subgraphs.contains(&key_target_node.source)
            {
                continue;
            }
            if let Some(found_edge_idx) = edge_finder(key_target) {
                let candidate = self.single_hop_candidate(
                    found_edge_idx,
                    key_edge,
                    key_target_node.source.clone(),
                    (&source_type, &source_schema),
                )?;
                KeyHopCandidate::insert_or_replace(&mut candidates, candidate);
            } else if !matches!(
                key_edge.transition,
                QueryGraphEdgeTransition::RootTypeResolution { .. }
            ) {
                need_chain.push((key_target, key_edge_idx));
            }
        }

        options.extend(self.evaluate_hop_candidates(pending_node, &candidates)?);

        // A hop with circular key conditions doesn't count as reaching: its
        // commit fails unless the anchor resolves the whole key, so a
        // satisfiable chain (e.g. through a subgraph keyed on a field the
        // state does have) must still be offered; ranking already prefers
        // chains over circular hops.
        if options.iter().all(|opt| opt.conditions_unroutable()) {
            // Each first hop runs its own shortest-chain search. Keep only the
            // globally shortest chains: stopping at the first first hop with
            // any chain can commit a detour through an extra subgraph.
            let mut chains = Vec::new();
            for (key_target, key_edge_idx) in need_chain {
                let key_edge = self
                    .cached_query_graph
                    .query_graph
                    .edge_weight(key_edge_idx)?;
                chains.extend(self.chained_key_hop_options(
                    pending_node,
                    key_target,
                    key_edge,
                    &current_source,
                    &source_type,
                    &source_schema,
                    edge_finder,
                )?);
            }
            let shortest = chains.iter().map(|c| c.intermediate_hops().len()).min();
            options.extend(
                chains
                    .into_iter()
                    .filter(|c| Some(c.intermediate_hops().len()) == shortest),
            );
        }
        Ok(())
    }

    /// Follow key edges transitively toward a node with the target field,
    /// producing multi-hop choices that capture the chain.
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
            (Some(conds), Some(st), Some(ss)) => self.can_satisfy(conds, st, ss),
            (None, _, _) => true,
            _ => false,
        };
        let mut first_conditions_unroutable = false;
        if !first_conditions_local && let Some(conds) = &first_key_edge.conditions {
            first_conditions_unroutable = !self.conditions_routable(origin_node, conds)?;
        }

        let mut visited: Vec<Arc<str>> = vec![
            origin_source.clone(),
            self.cached_query_graph
                .query_graph
                .node_weight(first_intermediate)?
                .source
                .clone(),
        ];

        // Breadth-first over key edges, each subgraph visited once, stop at
        // the first depth with hits so the returned chains are shortest.
        // This can inadmissibly prune a longer chain whose key conditions
        // are cheaper to resolve, but each extra hop adds a pipelined
        // fetch, so the shorter chain is almost always cheaper in practice.
        //
        // Chains are short (MAX_CHAIN_DEPTH) and few (one visit per
        // subgraph), so plain vec clones are fine.
        let mut frontier: VecDeque<Vec<IntermediateKeyHop>> =
            VecDeque::from([vec![IntermediateKeyHop {
                target_node: first_intermediate,
                entry_key: first_key_edge.conditions.clone(),
            }]]);
        let mut options = Vec::new();

        while let Some(hops) = frontier.pop_front() {
            let Some(current) = hops.last().map(|hop| hop.target_node) else {
                continue;
            };
            // FIXME: the JS planner uses loop detection instead of a fixed
            // depth limit, so long chains that are valid could be pruned here.
            if hops.len() >= MAX_CHAIN_DEPTH {
                continue;
            }
            for next in self.chain_exits(current, &mut visited)? {
                match edge_finder(next.target_node) {
                    Some(found_edge) => {
                        let edge = EdgeInfo {
                            edge_index: found_edge,
                            target_subgraph: self
                                .cached_query_graph
                                .query_graph
                                .node_weight(next.target_node)?
                                .source
                                .clone(),
                        };
                        let key = KeyHopInfo {
                            key_conditions: next.entry_key.ok_or_else(|| {
                                FederationError::internal("chained key hop missing entry key")
                            })?,
                            requires_resolvable_in_place: self
                                .requires_conditions_resolvable_in_place(origin_node, found_edge)?,
                            conditions_unroutable: first_conditions_unroutable,
                        };
                        if first_conditions_unroutable {
                            options.push(RoutingChoice::CircularKeyHop {
                                edge,
                                key,
                                intermediate_hops: hops.clone(),
                            });
                        } else {
                            options.push(RoutingChoice::ChainedKeyHop {
                                edge,
                                key,
                                intermediate_hops: hops.clone(),
                            });
                        }
                    }
                    None => {
                        let mut extended = hops.clone();
                        extended.push(next);
                        frontier.push_back(extended);
                    }
                }
            }
            // FIXME: stopping at the first depth with hits can miss a
            // longer chain whose key conditions are cheaper to resolve.
            if !options.is_empty() {
                break;
            }
        }
        Ok(options)
    }

    /// Entry-keyed hops over key-resolution edges from `current` into
    /// subgraphs not yet visited and not disabled; marks them visited.
    fn chain_exits(
        &self,
        current: NodeIndex,
        visited: &mut Vec<Arc<str>>,
    ) -> Result<Vec<IntermediateKeyHop>, FederationError> {
        let mut exits = Vec::new();
        for key_edge_idx in self
            .cached_query_graph
            .query_graph
            .out_edges(current)
            .into_iter()
            .map(|e| e.id())
        {
            let key_edge = self
                .cached_query_graph
                .query_graph
                .edge_weight(key_edge_idx)?;
            if !matches!(key_edge.transition, QueryGraphEdgeTransition::KeyResolution) {
                continue;
            }
            let (_, target) = self
                .cached_query_graph
                .query_graph
                .edge_endpoints(key_edge_idx)?;
            let subgraph = &self
                .cached_query_graph
                .query_graph
                .node_weight(target)?
                .source;
            if visited.contains(subgraph) || self.disabled_subgraphs.contains(subgraph) {
                continue;
            }
            visited.push(subgraph.clone());
            exits.push(IntermediateKeyHop {
                target_node: target,
                entry_key: key_edge.conditions.clone(),
            });
        }
        Ok(exits)
    }

    /// Dedup candidate for a single-hop key edge whose target has the
    /// field. Satisfiability is computed before dedup so a producible key
    /// never loses to an unsatisfiable same-subgraph rival.
    fn single_hop_candidate(
        &self,
        found_edge_idx: EdgeIndex,
        key_edge: &crate::query_graph::QueryGraphEdge,
        target_subgraph: Arc<str>,
        (source_type, source_schema): (
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
                (Some(conds), Some(st), Some(ss)) => self.can_satisfy(conds, st, ss),
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

    /// Convert dedup candidates into RoutingChoice values, computing
    /// condition routability for non-root, non-local candidates.
    fn evaluate_hop_candidates(
        &self,
        pending_node: NodeIndex,
        candidates: &[KeyHopCandidate],
    ) -> Result<Vec<RoutingChoice>, FederationError> {
        let mut options = Vec::with_capacity(candidates.len());
        for c in candidates {
            let mut conditions_unroutable = false;
            if !c.is_root
                && !c.conditions_local
                && let Some(conds) = &c.key_conditions
            {
                conditions_unroutable = !self.conditions_routable(pending_node, conds)?;
            }

            let edge = EdgeInfo {
                edge_index: c.found_edge_idx,
                target_subgraph: c.target_subgraph.clone(),
            };

            if c.is_root {
                trace!(
                    target_subgraph = %edge.target_subgraph,
                    is_root = true,
                    "found edge via key hop",
                );
                options.push(RoutingChoice::RootHop(edge));
                continue;
            }

            let key_conditions = match &c.key_conditions {
                Some(conds) => conds.clone(),
                None => {
                    return Err(FederationError::internal("key hop missing conditions"));
                }
            };

            trace!(
                target_subgraph = %edge.target_subgraph,
                conditions_local = c.conditions_local,
                conditions_unroutable,
                "found edge via key hop",
            );

            let key = KeyHopInfo {
                key_conditions,
                requires_resolvable_in_place: self
                    .requires_conditions_resolvable_in_place(pending_node, c.found_edge_idx)?,
                conditions_unroutable,
            };

            if conditions_unroutable {
                options.push(RoutingChoice::CircularKeyHop {
                    edge,
                    key,
                    intermediate_hops: Vec::new(),
                });
            } else if c.conditions_local {
                options.push(RoutingChoice::KeyHopWithLocalKey { edge, key });
            } else {
                options.push(RoutingChoice::KeyHopWithExternalKey { edge, key });
            }
        }
        Ok(options)
    }

    /// Enumerate `RoutingChoice`s for a pending selection at its query
    /// graph node.
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
            options.sort_by_key(RoutingChoice::rank);
            options
        };
        if !self.disabled_subgraphs.is_empty() {
            options.retain(|opt| !self.disabled_subgraphs.contains(opt.target_subgraph()));
        }
        Ok(options)
    }

    /// Options at the FederatedRootType head node, which fans out to
    /// per-subgraph roots via SubgraphEnteringTransition edges.
    pub(super) fn federated_root_options(
        &self,
        pending: &PendingSelection,
    ) -> Result<Vec<RoutingChoice>, FederationError> {
        let mut options = Vec::new();
        let field_selection = match &pending.selection {
            Selection::Field(field_selection) => field_selection,
            // Root types are object types, so any inline fragment here is
            // vacuous. It may still carry directives that need preserving.
            Selection::InlineFragment(_) => {
                options.push(RoutingChoice::StripFragment);
                return Ok(options);
            }
        };
        for edge in self
            .cached_query_graph
            .query_graph
            .subgraph_entering_transitions(pending.query_graph_node)
        {
            let subgraph_root = edge.target();
            if let Some(field_edge_idx) = self
                .cached_query_graph
                .edge_for_field(subgraph_root, &field_selection.field)
            {
                let subgraph_node = self
                    .cached_query_graph
                    .query_graph
                    .node_weight(subgraph_root)?;
                if self.disabled_subgraphs.contains(&subgraph_node.source) {
                    continue;
                }
                options.push(RoutingChoice::Local(EdgeInfo {
                    edge_index: field_edge_idx,
                    target_subgraph: subgraph_node.source.clone(),
                }));
            }
        }

        // Prefer root options which can locally satisfy more fields in the selection
        if options.len() > 1
            && let Some(sub_ss) = field_selection.selection_set.as_ref()
        {
            options.sort_by_cached_key(|opt| {
                let count = opt
                    .edge_index()
                    .and_then(|idx| self.cached_query_graph.query_graph.edge_endpoints(idx).ok())
                    .map(|(_, target)| self.count_local_sub_selections(target, sub_ss))
                    .unwrap_or(0);
                std::cmp::Reverse(count)
            });
        }

        Ok(options)
    }

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
            let target_node = self.cached_query_graph.query_graph.node_weight(target)?;
            let edge = self.cached_query_graph.query_graph.edge_weight(edge_idx)?;
            // @fromContext at a position the entity boundary does not already
            // isolate needs a same-subgraph entity re-entry so the context
            // value rides the representation. At the boundary the re-entry
            // stays as a fallback for when the fetch feeding the
            // representation cannot resolve the context fields.
            let needs_isolation = !edge.required_contexts.is_empty()
                && super::context::needs_context_isolation(pending, &edge.required_contexts);
            if edge.conditions.is_none() && edge.required_contexts.is_empty() {
                options.push(self.direct_choice(edge_idx, target_node.source.clone())?);
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
        self.append_key_hop_options(pending.query_graph_node, key, &mut options, |key_target| {
            self.cached_query_graph
                .edge_for_field(key_target, &field_selection.field)
        })?;

        let current_node_data = self
            .cached_query_graph
            .query_graph
            .node_weight(pending.query_graph_node)?;
        let is_abstract = matches!(
            CompositeTypeDefinitionPosition::try_from(current_node_data.type_.clone()),
            Ok(pos) if pos.is_abstract_type()
        );
        if is_abstract {
            options.push(RoutingChoice::TypeExplosion);
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

        // Under a shareable parent returning an inconsistent abstract type,
        // only intersection members may appear as fragment conditions —
        // others would make results depend on which subgraph resolved the
        // parent.
        if let Some(filter) = &pending.narrowing.intersection_filter
            && let Some(type_cond) = &fragment_selection.inline_fragment.type_condition_position
        {
            let cond_types = self
                .supergraph_schema
                .possible_runtime_types(type_cond.clone())?;
            if cond_types.iter().all(|t| !filter.contains(&t.type_name)) {
                return Ok(options);
            }
        }

        if let Some(edge_idx) = self.cached_query_graph.edge_for_inline_fragment(
            pending.query_graph_node,
            &fragment_selection.inline_fragment,
        ) {
            let (_, target) = self
                .cached_query_graph
                .query_graph
                .edge_endpoints(edge_idx)?;
            let target_node = self.cached_query_graph.query_graph.node_weight(target)?;
            options.push(RoutingChoice::Local(EdgeInfo {
                edge_index: edge_idx,
                target_subgraph: target_node.source.clone(),
            }));
        }

        let Some(type_cond) = &fragment_selection.inline_fragment.type_condition_position else {
            options.push(RoutingChoice::StripFragment);
            return Ok(options);
        };

        if self.is_vacuous_type_condition(pending.query_graph_node, type_cond)? {
            options.push(RoutingChoice::StripFragment);
        }

        // @interfaceObject fake downcast: the concrete type doesn't exist in
        // this subgraph.
        for edge_idx in self
            .cached_query_graph
            .query_graph
            .out_edges(pending.query_graph_node)
            .into_iter()
            .map(|e| e.id())
        {
            let edge_weight = self.cached_query_graph.query_graph.edge_weight(edge_idx)?;
            if let QueryGraphEdgeTransition::InterfaceObjectFakeDownCast { to_type_name, .. } =
                &edge_weight.transition
                && type_cond.type_name() == to_type_name
            {
                let (_, target) = self
                    .cached_query_graph
                    .query_graph
                    .edge_endpoints(edge_idx)?;
                let target_node = self.cached_query_graph.query_graph.node_weight(target)?;
                // Only offer the fake downcast when the target subgraph can
                // resolve at least one non-__typename field under it. A
                // downcast to a subgraph that owns none of the requested
                // fields would produce an empty fetch.
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
                    options.push(RoutingChoice::Local(EdgeInfo {
                        edge_index: edge_idx,
                        target_subgraph: target_node.source.clone(),
                    }));
                }
                break;
            }
        }

        trace!(
            type_condition = %type_cond.type_name(),
            "searching key hops for fragment downcast",
        );
        let key = RoutingSiteKey::InlineFragment(Some(type_cond.type_name().clone()));
        self.append_key_hop_options(pending.query_graph_node, key, &mut options, |key_target| {
            self.cached_query_graph
                .edge_for_inline_fragment(key_target, &fragment_selection.inline_fragment)
        })?;

        if type_cond.is_abstract_type() {
            options.push(RoutingChoice::TypeExplosion);
        }

        // When no routing option exists (no local edge, not vacuous, no
        // key hops, concrete type condition), offer StripFragment so the
        // commit path can detect unsatisfiable conditions (e.g., empty
        // local runtime intersection) and drop the fragment gracefully
        // instead of penalizing the plan with dropped_fields.
        if options.is_empty() {
            options.push(RoutingChoice::StripFragment);
        }

        Ok(options)
    }

    /// Whether the type condition is vacuous at the given node, meaning every
    /// runtime type at that position satisfies the condition.
    fn is_vacuous_type_condition(
        &self,
        node: NodeIndex,
        type_cond: &CompositeTypeDefinitionPosition,
    ) -> Result<bool, FederationError> {
        let current_node = self.cached_query_graph.query_graph.node_weight(node)?;
        if matches!(current_node.type_, QueryGraphNodeType::FederatedRootType(_)) {
            return Ok(true);
        }
        let current_type: CompositeTypeDefinitionPosition =
            current_node.type_.clone().try_into()?;
        let current_schema = self
            .cached_query_graph
            .query_graph
            .schema_by_source(&current_node.source)?;
        let current_runtime_types = current_schema.possible_runtime_types(current_type)?;
        let cond_runtime_types = self
            .supergraph_schema
            .possible_runtime_types(type_cond.clone())?;
        Ok(current_runtime_types.is_subset(&cond_runtime_types))
    }

    /// Enumerate key hops into a fresh Vec; the cycle guard lives in
    /// `append_key_hop_options`.
    pub(super) fn key_hops_guarded(
        &self,
        node: NodeIndex,
        key: RoutingSiteKey,
        edge_finder: impl Fn(NodeIndex) -> Option<EdgeIndex>,
    ) -> Result<Vec<RoutingChoice>, FederationError> {
        let mut hops = Vec::new();
        self.append_key_hop_options(node, key, &mut hops, edge_finder)?;
        Ok(hops)
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
        for sel in conditions.selections.values() {
            let routable = match sel {
                Selection::Field(field_sel) => self.condition_field_routable(node, field_sel)?,
                Selection::InlineFragment(frag_sel) => {
                    self.condition_fragment_routable(node, frag_sel)?
                }
            };
            if !routable {
                return Ok(false);
            }
        }
        Ok(true)
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
        // FIXME: a direct edge is taken as routable without checking the
        // edge's own @requires or @fromContext conditions, which may have no
        // route of their own. Commit re-checks them, so the cost is a late
        // drop rather than a wrong plan. The condition resolution rework
        // should account for edge conditions here.
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
        let hops = self.key_hops_guarded(node, key, |key_target| {
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
        // FIXME: a missing downcast edge does not only mean "hop elsewhere".
        // It can mean the type condition must be exploded into the runtime
        // types this node shares with it. This falls through to key hops
        // and may report unroutable where explosion would succeed.
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
        let key = RoutingSiteKey::InlineFragment(Some(type_cond.type_name().clone()));
        let hops = self.key_hops_guarded(node, key, |key_target| {
            self.cached_query_graph
                .edge_for_inline_fragment(key_target, &frag_sel.inline_fragment)
        })?;
        self.hops_reach(&hops, Some(&frag_sel.selection_set))
    }

    /// Whether any of `hops` can deliver `sub_ss`: at least one hop whose
    /// target routes the sub-selections.
    ///
    /// FIXME: this requires a single hop target to route the whole
    /// sub-selection, but a condition set can be served by calling the same
    /// field in several subgraphs for different parts of it. The subgraph
    /// jump simplification should relax this.
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
            if let Some(edge_idx) = hop.edge_index() {
                let (_, target) = self
                    .cached_query_graph
                    .query_graph
                    .edge_endpoints(edge_idx)?;
                if self.conditions_routable(target, sub_ss)? {
                    return Ok(true);
                }
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

    /// True when every descendant field is local, so the whole subtree
    /// can be added in one shot.
    #[allow(dead_code)]
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
    /// node.
    #[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use apollo_compiler::name;

    use super::super::test_support;
    use super::*;

    fn search_space() -> FieldRoutingSearchSpace {
        test_support::search_space(&[
            (
                "S1",
                r#"
                type Query { t: T }
                type T @key(fields: "k") { k: ID, x: Int }
                "#,
            ),
            (
                "S2",
                r#"
                type T @key(fields: "k") { k: ID, y: Int }
                "#,
            ),
        ])
    }

    fn key_selection(space: &FieldRoutingSearchSpace, text: &str) -> Arc<SelectionSet> {
        let schema = space
            .cached_query_graph
            .query_graph
            .schema_by_source("S1")
            .expect("S1 schema")
            .clone();
        let t_pos: CompositeTypeDefinitionPosition = schema
            .get_type(&name!("T"))
            .expect("T exists")
            .try_into()
            .expect("T is composite");
        Arc::new(SelectionSet::parse(schema, t_pos, text).expect("key parses"))
    }

    fn any_key_edge(space: &FieldRoutingSearchSpace) -> EdgeIndex {
        space
            .cached_query_graph
            .query_graph
            .graph()
            .edge_indices()
            .find(|&idx| {
                matches!(
                    space.cached_query_graph.query_graph.graph()[idx].transition,
                    QueryGraphEdgeTransition::KeyResolution,
                )
            })
            .expect("composed graph has a key edge")
    }

    /// Satisfiable beats provided beats unsatisfiable, then smaller keys win.
    #[test]
    fn key_hops_sort_by_satisfiability_then_key_size() {
        let space = search_space();
        let edge = any_key_edge(&space);

        let satisfiable = RoutingChoice::KeyHopWithLocalKey {
            edge: EdgeInfo {
                edge_index: edge,
                target_subgraph: Arc::from("S1"),
            },
            key: KeyHopInfo {
                key_conditions: key_selection(&space, "k x"),
                requires_resolvable_in_place: false,
                conditions_unroutable: false,
            },
        };
        let provided = RoutingChoice::KeyHopWithProvidedKey {
            edge: EdgeInfo {
                edge_index: edge,
                target_subgraph: Arc::from("S2"),
            },
            key: KeyHopInfo {
                key_conditions: key_selection(&space, "k"),
                requires_resolvable_in_place: false,
                conditions_unroutable: false,
            },
        };
        let unsatisfiable_small = RoutingChoice::KeyHopWithExternalKey {
            edge: EdgeInfo {
                edge_index: edge,
                target_subgraph: Arc::from("S3"),
            },
            key: KeyHopInfo {
                key_conditions: key_selection(&space, "k"),
                requires_resolvable_in_place: true,
                conditions_unroutable: false,
            },
        };
        let unsatisfiable_large = RoutingChoice::KeyHopWithExternalKey {
            edge: EdgeInfo {
                edge_index: edge,
                target_subgraph: Arc::from("S4"),
            },
            key: KeyHopInfo {
                key_conditions: key_selection(&space, "k x"),
                requires_resolvable_in_place: false,
                conditions_unroutable: false,
            },
        };

        let mut options = [
            unsatisfiable_large,
            provided,
            unsatisfiable_small,
            satisfiable,
        ];
        options.sort_by_key(RoutingChoice::rank);
        assert_eq!(
            options[0].target_subgraph().as_ref(),
            "S1",
            "satisfiable wins"
        );
        assert_eq!(options[1].target_subgraph().as_ref(), "S2", "provided next");
        assert_eq!(
            options[2].target_subgraph().as_ref(),
            "S3",
            "unsatisfiable small key"
        );
        assert_eq!(
            options[3].target_subgraph().as_ref(),
            "S4",
            "unsatisfiable large key last"
        );
    }
}
