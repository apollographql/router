//! Source-aware query planning — Phase 1: the connect-graph-builder (first slice).
//!
//! Bridges the Spike-A connector-condition derivation
//! ([`crate::connectors::source_aware`]) into the canonical query-graph edge
//! model. The proposal's target design reuses the existing
//! [`QueryGraphEdgeTransition::KeyResolution`] transition for connector-entering
//! edges (a new transition variant would fan out across ~7 exhaustive matches),
//! and models a connector's parent-data requirement as the edge's
//! `conditions: Option<Arc<SelectionSet>>` — exactly how `@key`/`@requires`
//! conditions are already represented (`build_query_graph.rs:1340`).
//!
//! The first slice produced the edge *data* — the transition, the landing type,
//! and the condition `SelectionSet` — for each connector, without mutating a
//! live [`QueryGraph`](super::QueryGraph), letting the condition-conversion be
//! differentially tested against the Spike-A output.
//!
//! The second slice, [`restrict_connector_reachability`], is the "restrictive
//! provides" pass (see `SOURCE_AWARE_B2B_RESTRICTIVE_PROVIDES.md`): a post-build
//! graph mutation that reconstructs the per-connector field boundary expansion
//! encoded structurally as minimal synthetic subgraphs. For each connector field
//! edge it copies the landing-type node, prunes the copy's field edges to the
//! fields the connector's `selection` actually returns, and re-points the field
//! edge to the copy — leaving pruned fields reachable only through the copy's
//! `KeyResolution` re-entry edges, so the planner emits a proper `_entities`
//! fetch (with its existing, correct key/validity logic) instead of over-merging
//! a field into a fetch the connector cannot serve.

// Phase-1 seam: `SourceEnteringEdge` fields are consumed when the edge data is
// grafted into `build_query_graph`; until then they're read only by the tests.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::ast::NamedType;
use petgraph::Direction;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::QueryGraph;
use super::QueryGraphEdge;
use super::QueryGraphEdgeTransition;
use super::QueryGraphNode;
use super::QueryGraphNodeType;
use super::build_query_graph::precompute_non_trivial_followup_edges;
use crate::connectors::Connector;
use crate::connectors::EntityResolver;
use crate::connectors::source_aware::derive_condition;
use crate::error::FederationError;
use crate::operation::SelectionSet;
use crate::operation::merge_selection_sets;
use crate::query_plan::connector_stamp::ProvidedTree;
use crate::query_plan::connector_stamp::connector_provided_tree;
use crate::query_plan::query_planning_traversal::non_local_selections_estimation::precompute_non_local_selection_metadata;
use crate::schema::FederationSchema;
use crate::schema::ValidFederationSchema;
use crate::schema::field_set::parse_field_set;
use crate::supergraph::extract_subgraphs_from_supergraph;

/// The query-graph edge that *enters* a connector's source — the data a
/// `SourceEntering`/`KeyResolution` edge carries in the source-aware graph.
///
/// This is deliberately the shape of the canonical edge (transition +
/// conditions), so wiring it into `build_query_graph` is a direct `add_edge`.
#[derive(Debug)]
pub(crate) struct SourceEnteringEdge {
    /// The connector this edge dispatches to (direct id, no synthetic service name).
    pub(crate) connector_coordinate: String,
    /// The connector's entity-resolver kind.
    pub(crate) entity_resolver: Option<EntityResolver>,
    /// The type the condition (and the entity key) is rooted on.
    pub(crate) key_type: NamedType,
    /// The type the connector produces (its base type), when determinable.
    pub(crate) output_type: Option<NamedType>,
    /// Canonical transition kind — reused, not a new variant.
    pub(crate) transition: QueryGraphEdgeTransition,
    /// The parent-data condition guarding the edge, as a query-graph
    /// `SelectionSet` (converted from the Spike-A `FieldSet`).
    pub(crate) conditions: Option<Arc<SelectionSet>>,
}

/// Build the source-entering edge for `connector`, or `Ok(None)` when the
/// connector contributes no parent-data condition (a plain root-field connector,
/// or a legacy implicit resolver with no key — see
/// [`derive_condition`]). Those non-keyed cases are ordinary field-collection
/// edges and are out of scope for this slice.
pub(crate) fn build_source_entering_edge(
    connector: &Connector,
    schema: &ValidFederationSchema,
) -> Result<Option<SourceEnteringEdge>, FederationError> {
    let Some(condition) =
        derive_condition(connector, schema.schema()).map_err(FederationError::internal)?
    else {
        return Ok(None);
    };

    // The condition is already rooted on the entity/key type — take that type
    // straight off the FieldSet rather than re-deriving the resolver's key type.
    let key_type = condition.selection_set.ty.clone();
    let fields = condition.serialize().no_indent().to_string();

    // Reuse the exact conversion `build_query_graph` uses for @key conditions.
    let conditions = Arc::new(parse_field_set(schema, key_type.clone(), &fields, true)?);

    Ok(Some(SourceEnteringEdge {
        connector_coordinate: connector.id.coordinate(),
        entity_resolver: connector.entity_resolver.clone(),
        output_type: connector.id.directive.base_type_name(schema.schema()),
        key_type,
        transition: QueryGraphEdgeTransition::KeyResolution,
        conditions: Some(conditions),
    }))
}

/// Collect the source-entering edges for **every** connector in a
/// (non-expanded) supergraph — the full set the source-aware builder will add
/// to the query graph.
///
/// Extracts the supergraph's subgraphs *without* connector expansion, builds
/// each subgraph's connectors, and gathers a [`SourceEnteringEdge`] per keyed
/// connector. Connectors that contribute no condition (plain root fields,
/// legacy implicit-without-key) are skipped — see [`build_source_entering_edge`].
pub(crate) fn build_connector_source_edges(
    supergraph_schema: &FederationSchema,
) -> Result<Vec<SourceEnteringEdge>, FederationError> {
    let subgraphs = extract_subgraphs_from_supergraph(supergraph_schema, Some(true))?;

    let mut edges = Vec::new();
    for (_, subgraph) in subgraphs.subgraphs.iter() {
        let schema = &subgraph.schema;
        // A subgraph with no connect link yields no connectors — skip it.
        let Ok(connectors) = Connector::from_schema(schema.schema(), &subgraph.name) else {
            continue;
        };
        for connector in &connectors {
            if let Some(edge) = build_source_entering_edge(connector, schema)? {
                edges.push(edge);
            }
        }
    }
    Ok(edges)
}

/// Graft each implicit-resolver connector's `$this` reads onto its own
/// field-collecting edge as a planner condition — the source-aware counterpart
/// of the `@key` expansion derives for the connector's synthetic subgraph.
///
/// A connector's mapping expressions can read sibling fields of its parent type
/// (`User.d` as `GET /{$this.c}/d` with `body: "with_b: $this.b"`), and those
/// reads are a data dependency: the fetch cannot be dispatched until they are
/// resolved. Expansion does not learn this from `@join__field(requires:)` — it
/// calls [`Connector::resolvable_key`], which reads
/// [`variable_references`](Connector::variable_references) across transport
/// *and* selection, and emits `key: "b c"` on the synthetic subgraph. Planning
/// over the raw graph, nothing encodes those reads, so the planner satisfies
/// only the declared `requires` and dispatches the connector without `b`.
///
/// This applies the same derivation ([`derive_condition`], which mirrors
/// `resolvable_key`) to the raw graph, in exactly the representation
/// `handle_requires` uses for `@requires`: a `SelectionSet` on the field edge's
/// `conditions`, parsed against the supergraph schema and merged with whatever
/// `@requires` already put there.
///
/// Scoped to [`EntityResolver::Implicit`] — a `Foo.bar @connect` reading
/// `$this.*` from its own parent. The other resolver kinds derive a key rooted
/// on the connector's *output* type (`$args` for an explicit `Query.foo`
/// resolver, `$this`/`$batch` for a type-level one), which is a re-entry key
/// rather than a requirement on the parent, and belongs on the re-entry edge the
/// restrictive-provides pass already creates. The `__typename` singleton
/// [`derive_condition`] fabricates for an implicit resolver with no `$this`
/// reads is skipped too: it states no parent requirement.
///
/// **Not currently wired into [`SourceAwareQueryPlanner`], and the reason is the
/// finding.** With this applied the planner does fetch the reads, but
/// `correctness::check_plan` then rejects the plan, correctly: `@requires` names
/// **external** fields, and a sibling field of the same subgraph is not
/// external, so the raw supergraph has no way to declare the dependency this
/// pass infers. Expansion escapes that by splitting each connector into its own
/// synthetic subgraph, which makes the sibling dependency cross-subgraph and
/// therefore expressible as a key. Where intra-`connectors` field dependencies
/// should live is an open design question; this function is the graph half of
/// whatever the answer turns out to be. See
/// `source_aware::tests::plans_this_variable_reads_as_fetch_inputs`.
///
/// Returns whether the graph was mutated.
pub(crate) fn apply_connector_parent_conditions(
    query_graph: &mut QueryGraph,
    supergraph_schema: &ValidFederationSchema,
    connectors: &[Connector],
) -> Result<bool, FederationError> {
    if connectors.is_empty() {
        return Ok(false);
    }

    // (edge, condition) pairs, collected before mutating so the borrow of the
    // graph's edge weights ends first.
    let mut grafts: Vec<(EdgeIndex, SelectionSet)> = Vec::new();

    for edge in query_graph.graph.edge_indices() {
        let edge_weight = query_graph.edge_weight(edge)?;
        let QueryGraphEdgeTransition::FieldCollection {
            source,
            field_definition_position,
            ..
        } = &edge_weight.transition
        else {
            continue;
        };
        if *source == query_graph.current_source {
            continue;
        }
        let simple_name = format!(
            "{}.{}",
            field_definition_position.type_name(),
            field_definition_position.field_name()
        );
        let parent_type = field_definition_position.parent().type_name().clone();

        // Every connector on this field in this subgraph. With several variants
        // on one field (`[0]`, `[1]`, ...) the planner cannot tell them apart, so
        // require the union: any field some variant reads must be available.
        let mut all_conditions = Vec::new();
        for connector in connectors {
            if connector.id.subgraph_name.as_str() != source.as_ref()
                || connector.id.directive.simple_name() != simple_name
                || !matches!(connector.entity_resolver, Some(EntityResolver::Implicit))
            {
                continue;
            }
            let Some(condition) = derive_condition(connector, supergraph_schema.schema())
                .map_err(FederationError::internal)?
            else {
                continue;
            };
            // The fabricated `__typename` singleton is not a parent requirement.
            if condition
                .selection_set
                .selections
                .iter()
                .all(|s| s.as_field().is_some_and(|f| f.name == "__typename"))
            {
                continue;
            }
            all_conditions.push(parse_field_set(
                supergraph_schema,
                parent_type.clone(),
                &condition.serialize().no_indent().to_string(),
                true,
            )?);
        }
        if all_conditions.is_empty() {
            continue;
        }
        // Fold in whatever `@requires` already placed on this edge — the two
        // describe the same thing (parent data this field needs) and the
        // connector's reads are usually a superset, but neither subsumes the
        // other by construction.
        if let Some(existing) = &edge_weight.conditions {
            all_conditions.push((**existing).clone());
        }
        let merged = if all_conditions.len() == 1 {
            all_conditions.remove(0)
        } else {
            merge_selection_sets(all_conditions)?
        };
        grafts.push((edge, merged));
    }

    if grafts.is_empty() {
        return Ok(false);
    }

    let grafted = grafts.len();
    for (edge, conditions) in grafts {
        query_graph.edge_weight_mut(edge)?.conditions = Some(Arc::new(conditions));
    }

    tracing::debug!(
        edges = grafted,
        "source-aware: grafted connector $this reads onto field edges as conditions"
    );

    // Conditions do not change topology, but the traversal-layer maps are
    // precomputed at the end of graph building and the planner reads them, so
    // refresh them rather than reason about which ones consult conditions.
    precompute_non_trivial_followup_edges(query_graph)?;
    query_graph.non_local_selection_metadata =
        precompute_non_local_selection_metadata(query_graph)?;
    Ok(true)
}

/// The source-aware "restrictive provides" pass: reconstruct, in the query
/// graph, the per-connector field boundary that expansion built as minimal
/// synthetic subgraphs (see the module docs and
/// `SOURCE_AWARE_B2B_RESTRICTIVE_PROVIDES.md`).
///
/// For each `FieldCollection` edge backed by a connector, the connector's output
/// shape is read **recursively** ([`connector_provided_tree`]) and walked
/// alongside the graph from the landing-type node down. At every level where the
/// node has field edges the shape does not return at *that position*
/// ([`restrict_node`]):
///
/// 1. **Copy** the node (mirroring `copy_for_provides_internal`, including the
///    types-to-nodes bookkeeping), marked
///    [`connector_boundary_copy`](QueryGraphNode::connector_boundary_copy) so
///    path traversal permits its same-source re-entry.
/// 2. **Clone** the node's out-edges onto the copy, **pruning** field edges for
///    non-provided fields (keeping `__typename`), and **re-pointing** provided
///    fields whose own type was restricted at that type's restricted copy rather
///    than the shared full node. The original's self-key edge (head == tail,
///    added by `handle_key` for every keyed type) clones into `copy -> original`
///    — exactly the `KeyResolution` re-entry that keeps pruned fields reachable.
///    True self-edges are ignored by the planner, but this copy-to-original edge
///    joins distinct nodes and is considered.
/// 3. **Re-point** the connector's field edge to the outermost copy (mirroring
///    `update_edge_tail`: paired add-then-remove preserves edge indices).
///
/// Recursing is what distinguishes one type reached at two positions with
/// different sub-selections. A connector selecting
/// `id manager { id name } reports { id }` reaches `Person` under `manager` with
/// `{id, name}` and under `reports` with `{id}`; a top-level-only read sees only
/// `User`'s fields, all of which are provided, and so prunes nothing at all while
/// the planner goes on believing the connector serves `Person.name` at both
/// positions.
///
/// The planner keeps ownership of *validity*: it emits an `_entities` fetch for
/// a pruned field only when the re-entry's key condition is actually satisfiable
/// from the copy, and correctly fails otherwise — no hand-rolled
/// missing-field-to-`_entities` translation.
///
/// Conservatively leaves a level alone when nothing is prunable there (the shape
/// provides every field) or when that node has no `KeyResolution` re-entry
/// (pruning would only turn today's over-merge into a planning error — see the
/// follow-on plan's "no-key semantics" item, which is an operator decision). A
/// level that prunes nothing itself but has a restricted child is still copied,
/// to carry the re-pointed child edge, and needs no re-entry of its own. Only
/// source-aware raw graphs contain unexpanded connectors, so the expansion path
/// never reaches the mutation.
///
/// Returns whether the graph was mutated. On mutation, recomputes the
/// traversal-layer maps (`non_trivial_followup_edges`,
/// `non_local_selection_metadata`) that were precomputed at the end of graph
/// building — traversal hard-errors on edges missing from the former and would
/// never consider new edges absent from its followup lists.
pub(crate) fn restrict_connector_reachability(
    query_graph: &mut QueryGraph,
    connectors: &[Connector],
) -> Result<bool, FederationError> {
    if connectors.is_empty() {
        return Ok(false);
    }

    struct Candidate {
        edge: EdgeIndex,
        landing: NodeIndex,
        provided: ProvidedTree,
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    for edge in query_graph.graph.edge_indices() {
        let edge_weight = query_graph.edge_weight(edge)?;
        let QueryGraphEdgeTransition::FieldCollection {
            source,
            field_definition_position,
            ..
        } = &edge_weight.transition
        else {
            continue;
        };
        let simple_name = format!(
            "{}.{}",
            field_definition_position.type_name(),
            field_definition_position.field_name()
        );
        // All connectors on this field in this subgraph. With several connectors
        // on one field ([0], [1], ...) prune only what *no* variant provides
        // (the merge) — conservative, since the planner cannot distinguish them.
        let mut provided: Option<ProvidedTree> = None;
        let mut skip = false;
        for connector in connectors {
            if connector.id.subgraph_name.as_str() != source.as_ref()
                || connector.id.directive.simple_name() != simple_name
            {
                continue;
            }
            let Some(tree) = connector_provided_tree(connector) else {
                // Non-object output shape (e.g. a scalar field connector) — not
                // the root/entity over-merge case this pass guards.
                skip = true;
                break;
            };
            match &mut provided {
                Some(existing) => existing.merge(tree),
                None => provided = Some(tree),
            }
        }
        let Some(provided) = provided.filter(|_| !skip) else {
            continue;
        };

        let (_, landing) = query_graph.edge_endpoints(edge)?;
        if !is_restrictable(query_graph, landing, source)? {
            continue;
        }

        candidates.push(Candidate {
            edge,
            landing,
            provided,
        });
    }

    if candidates.is_empty() {
        return Ok(false);
    }

    let mut memo: HashMap<(NodeIndex, ProvidedTree), Option<NodeIndex>> = HashMap::new();
    let mut copies_made = 0usize;

    for Candidate {
        edge,
        landing,
        provided,
    } in candidates
    {
        let Some(copy) =
            restrict_node(query_graph, landing, &provided, &mut memo, &mut copies_made)?
        else {
            // Nothing is prunable anywhere in this connector's output shape, or
            // no level that could be pruned has a re-entry — leave the graph
            // alone rather than trade an over-merge for a planning error.
            continue;
        };

        // Re-point the connector's field edge to the (recursively) restricted
        // copy (mirroring `update_edge_tail`: paired add-then-remove preserves
        // edge indices).
        let (head, _) = query_graph.edge_endpoints(edge)?;
        let weight = query_graph.edge_weight(edge)?.clone();
        query_graph.graph.add_edge(head, copy, weight);
        query_graph.graph.remove_edge(edge);
    }

    if copies_made == 0 {
        return Ok(false);
    }

    tracing::debug!(
        copies = copies_made,
        "restrictive-provides: created connector boundary copies"
    );

    precompute_non_trivial_followup_edges(query_graph)?;
    query_graph.non_local_selection_metadata =
        precompute_non_local_selection_metadata(query_graph)?;
    // Boundary copies invalidate the "rest of the selection is local to this subgraph" planning
    // shortcut for themselves *and every ancestor*, since that shortcut can fire at the root and
    // skip path-building for the whole operation. See
    // `QueryGraph::nodes_reaching_connector_boundary_copy`.
    query_graph.recompute_nodes_reaching_connector_boundary_copy();
    Ok(true)
}

/// Whether `node` is a node this pass may copy and prune: an object-ish schema
/// type in `source`, and not already a boundary copy (copies are only ever
/// reached from copies, and re-restricting one would prune an already-pruned
/// field set).
fn is_restrictable(
    query_graph: &QueryGraph,
    node: NodeIndex,
    source: &Arc<str>,
) -> Result<bool, FederationError> {
    let weight = query_graph.node_weight(node)?;
    Ok(weight.source == *source
        && !weight.connector_boundary_copy
        && matches!(weight.type_, QueryGraphNodeType::SchemaType(_)))
}

/// Whether `node` has a `KeyResolution` out-edge, i.e. whether a field pruned
/// from a copy of it stays reachable through an `_entities` re-entry.
fn has_reentry(query_graph: &QueryGraph, node: NodeIndex) -> bool {
    query_graph
        .graph
        .edges_directed(node, Direction::Outgoing)
        .any(|edge| {
            matches!(
                edge.weight().transition,
                QueryGraphEdgeTransition::KeyResolution
            )
        })
}

/// Build a copy of `node` restricted to `provided`, recursively restricting the
/// types of the provided fields that carry a sub-selection. Returns `Ok(None)`
/// when no restriction is called for anywhere beneath `node`, in which case the
/// caller should keep pointing at `node` itself.
///
/// Memoized on `(node, provided)`: one connector selection can reach the same
/// type at several positions with *different* sub-shapes — which is the entire
/// point of this pass — so the key must include the restriction, not just the
/// type. Two structurally identical restrictions of one node do share a copy.
///
/// Termination is by the `provided` tree, not the graph: each recursive step
/// descends one level into a finite tree (bounded by `MAX_PROVIDED_DEPTH` during
/// its derivation), so a cyclic type graph cannot cause unbounded recursion.
fn restrict_node(
    query_graph: &mut QueryGraph,
    node: NodeIndex,
    provided: &ProvidedTree,
    memo: &mut HashMap<(NodeIndex, ProvidedTree), Option<NodeIndex>>,
    copies_made: &mut usize,
) -> Result<Option<NodeIndex>, FederationError> {
    let memo_key = (node, provided.clone());
    if let Some(cached) = memo.get(&memo_key) {
        return Ok(*cached);
    }

    let node_weight = query_graph.node_weight(node)?.clone();
    let QueryGraphNodeType::SchemaType(type_pos) = &node_weight.type_ else {
        memo.insert(memo_key, None);
        return Ok(None);
    };

    // Snapshot the out-edges before any mutation: the recursion below adds nodes
    // and edges, and only ever reads *originals*, never a copy in progress.
    let out_edges: Vec<(NodeIndex, QueryGraphEdge)> = query_graph
        .graph
        .edges_directed(node, Direction::Outgoing)
        .map(|edge_ref| (edge_ref.target(), edge_ref.weight().clone()))
        .collect();

    // Restrict the children first, so this level only needs a copy when either
    // it prunes something itself or one of its children was restricted.
    let mut prune_here = false;
    let mut restricted_children: HashMap<String, NodeIndex> = HashMap::new();
    for (target, weight) in &out_edges {
        let QueryGraphEdgeTransition::FieldCollection {
            field_definition_position,
            ..
        } = &weight.transition
        else {
            continue;
        };
        let name = field_definition_position.field_name();
        if name.as_str() == "__typename" {
            continue;
        }
        if !provided.provides(name.as_str()) {
            prune_here = true;
            continue;
        }
        let Some(sub_tree) = provided.sub_tree(name.as_str()) else {
            // Provided with nothing restrictable below (a scalar leaf, or a
            // shape the derivation could not read as one object).
            continue;
        };
        if !is_restrictable(query_graph, *target, &node_weight.source)? {
            continue;
        }
        let sub_tree = sub_tree.clone();
        if let Some(child) = restrict_node(query_graph, *target, &sub_tree, memo, copies_made)? {
            restricted_children.insert(name.to_string(), child);
        }
    }

    // Per-level re-entry conservatism: pruning a field with no `KeyResolution`
    // re-entry would turn today's over-merge (a silent null) into a planning
    // error. That is arguably the more honest outcome, but it is a behaviour
    // change and belongs to the operator — see follow-on item 2, "no-key
    // semantics". Until it is decided, such a level keeps all of its fields.
    if prune_here && !has_reentry(query_graph, node) {
        // Record it: every such position is one where the over-merge survives, and that set is
        // precisely the evidence the no-key-semantics decision needs. Emitting it now means the
        // decision can be made from data rather than requiring another pass to collect it.
        tracing::debug!(
            type_ = %type_pos.type_name(),
            source = %node_weight.source,
            provided = ?provided.field_names().collect::<Vec<_>>(),
            "restrictive-provides: leaving an over-merge in place — fields are prunable here but \
             the type has no KeyResolution re-entry, so pruning would turn a silent null into a \
             planning error (follow-on: no-key semantics)"
        );
        prune_here = false;
    }

    if !prune_here && restricted_children.is_empty() {
        memo.insert(memo_key, None);
        return Ok(None);
    }

    // 1. Copy the node.
    let copy = query_graph.graph.add_node(QueryGraphNode {
        type_: node_weight.type_.clone(),
        source: node_weight.source.clone(),
        has_reachable_cross_subgraph_edges: node_weight.has_reachable_cross_subgraph_edges,
        provide_id: None,
        root_kind: None,
        connector_boundary_copy: true,
    });
    query_graph
        .types_to_nodes_by_source
        .get_mut(&node_weight.source)
        .ok_or_else(|| {
            FederationError::internal(format!(
                "Types-to-nodes map unexpectedly missing for source \"{}\"",
                node_weight.source
            ))
        })?
        .entry(type_pos.type_name().clone())
        .or_default()
        .insert(copy);
    *copies_made += 1;

    // 2. Clone out-edges onto the copy: prune the field edges this level does
    //    not provide, and re-point provided fields whose type was restricted at
    //    the copy of that type rather than the shared full node.
    //
    //    Non-field edges clone unchanged. That includes the original's self-key
    //    edge (head == tail, added by `handle_key` for every keyed type), which
    //    clones into `copy -> original` — exactly the `KeyResolution` re-entry
    //    that keeps pruned fields reachable. True self-edges are ignored by the
    //    planner, but this copy-to-original edge joins distinct nodes and is
    //    considered.
    for (target, weight) in out_edges {
        let mut target = target;
        if let QueryGraphEdgeTransition::FieldCollection {
            field_definition_position,
            ..
        } = &weight.transition
        {
            let name = field_definition_position.field_name();
            if name.as_str() != "__typename" {
                if prune_here && !provided.provides(name.as_str()) {
                    continue;
                }
                if let Some(child) = restricted_children.get(name.as_str()) {
                    target = *child;
                }
            }
        }
        query_graph.graph.add_edge(copy, target, weight);
    }

    memo.insert(memo_key, Some(copy));
    Ok(Some(copy))
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use insta::glob;

    use super::build_connector_source_edges;
    use super::build_source_entering_edge;
    use super::restrict_connector_reachability;
    use crate::connectors::Connector;
    use crate::query_graph::QueryGraphEdgeTransition;
    use crate::schema::FederationSchema;
    use crate::schema::ValidFederationSchema;
    use crate::supergraph::extract_subgraphs_from_supergraph;

    /// Normalize a selection rendering to a whitespace- and brace-insensitive
    /// token form: the query-graph `SelectionSet` renders with outer braces
    /// (`{ id }`) while the `FieldSet` renders without (`id`), but the field
    /// tokens between them must match.
    fn tokens(s: &str) -> Vec<String> {
        s.split_whitespace()
            .filter(|t| *t != "{" && *t != "}")
            .map(|t| t.to_string())
            .collect()
    }

    /// Every connector with a Spike-A condition yields a `KeyResolution`
    /// source-entering edge whose condition `SelectionSet` faithfully carries
    /// the derived key — proving the Spike-A condition converts losslessly into
    /// the canonical query-graph edge representation.
    #[test]
    fn source_entering_edges_carry_the_derived_condition() {
        let mut edges_built = 0usize;

        insta::with_settings!({prepend_module_to_snapshot => false}, {
            glob!("../connectors/expand/tests/schemas/expand", "*.graphql", |path| {
                let sdl = std::fs::read_to_string(path).unwrap();
                let supergraph_schema =
                    FederationSchema::new(Schema::parse(&sdl, "supergraph.graphql").unwrap())
                        .unwrap();
                let Ok(subgraphs) =
                    extract_subgraphs_from_supergraph(&supergraph_schema, Some(true))
                else {
                    return;
                };

                for (_, subgraph) in subgraphs.subgraphs.iter() {
                    let valid_schema: &ValidFederationSchema = &subgraph.schema;
                    let Ok(connectors) =
                        Connector::from_schema(valid_schema.schema(), &subgraph.name)
                    else {
                        continue;
                    };

                    for connector in &connectors {
                        // Skip connectors whose key derivation errors on this
                        // fixture — not a builder bug.
                        if connector.resolvable_key(valid_schema.schema()).is_err() {
                            continue;
                        }

                        let edge = build_source_entering_edge(connector, valid_schema)
                            .unwrap_or_else(|e| {
                                panic!("edge build failed for {} in {path:?}: {e}",
                                    connector.id.coordinate())
                            });

                        match edge {
                            Some(edge) => {
                                assert_eq!(
                                    edge.transition,
                                    QueryGraphEdgeTransition::KeyResolution,
                                    "connector-entering edge must reuse KeyResolution for {} in {path:?}",
                                    connector.id.coordinate()
                                );
                                let conditions = edge.conditions.expect(
                                    "a source-entering edge always carries a condition",
                                );
                                assert!(
                                    !conditions.is_empty(),
                                    "empty condition for {} in {path:?}",
                                    connector.id.coordinate()
                                );

                                // The condition SelectionSet must carry the same
                                // fields the Spike-A derivation produced.
                                let derived = crate::connectors::source_aware::derive_condition(
                                    connector,
                                    valid_schema.schema(),
                                )
                                .unwrap()
                                .unwrap();
                                assert_eq!(
                                    tokens(&conditions.to_string()),
                                    tokens(&derived.serialize().no_indent().to_string()),
                                    "condition field mismatch for {} in {path:?}",
                                    connector.id.coordinate()
                                );
                                edges_built += 1;
                            }
                            None => {
                                // No condition ⇒ no key. Consistent with Spike A
                                // deriving None for this connector.
                                assert!(
                                    crate::connectors::source_aware::derive_condition(
                                        connector,
                                        valid_schema.schema()
                                    )
                                    .unwrap()
                                    .is_none(),
                                    "edge was None but a condition was derived for {} in {path:?}",
                                    connector.id.coordinate()
                                );
                            }
                        }
                    }
                }
            });
        });

        assert!(
            edges_built > 0,
            "no source-entering edges built — did the fixture glob match?"
        );
    }

    /// DISTANCE PROBE (spike diagnostic, not an assertion of desired behavior).
    ///
    /// For each fixture, measure three things and print them:
    /// 1. Can we even build a federated query graph from the **raw**
    ///    (non-expanded) connector supergraph? (If not, that's a hard gap.)
    /// 2. The **expanded** supergraph's query graph — today's correct target:
    ///    total nodes/edges and how many are `KeyResolution`.
    /// 3. How many source-entering edges my source-aware builder derives from
    ///    the raw supergraph.
    ///
    /// The point is the console output, read during the spike — hence the loose
    /// asserts. Run with: `cargo test -p apollo-federation distance_probe -- --nocapture`.
    #[test]
    fn distance_probe_raw_vs_expanded_graph() {
        use crate::ApiSchemaOptions;
        use crate::Supergraph;
        use crate::connectors::expand::ExpansionResult;
        use crate::connectors::expand::expand_connectors;
        use crate::query_graph::build_query_graph::build_federated_query_graph;

        fn key_edges(graph: &crate::query_graph::QueryGraph) -> usize {
            graph
                .graph()
                .edge_weights()
                .filter(|e| e.transition == QueryGraphEdgeTransition::KeyResolution)
                .count()
        }

        insta::with_settings!({prepend_module_to_snapshot => false}, {
            glob!("../connectors/expand/tests/schemas/expand", "*.graphql", |path| {
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                let sdl = std::fs::read_to_string(path).unwrap();

                // (3) source-aware edges derived from the raw supergraph.
                let raw_fed = FederationSchema::new(
                    Schema::parse(&sdl, "supergraph.graphql").unwrap(),
                );
                let my_edges = raw_fed
                    .as_ref()
                    .ok()
                    .and_then(|fed| build_connector_source_edges(fed).ok())
                    .map(|e| e.len());

                // (1) raw connector supergraph → federated query graph?
                let raw_graph = Supergraph::new_with_router_specs(&sdl).ok().and_then(|sg| {
                    let api = sg.to_api_schema(ApiSchemaOptions::default()).ok()?;
                    build_federated_query_graph(sg.schema.clone(), api, Some(false), Some(true)).ok()
                });

                // (2) expanded supergraph → federated query graph (today's target).
                let expanded_graph = match expand_connectors(
                    &sdl,
                    &ApiSchemaOptions { include_defer: true, ..Default::default() },
                ) {
                    Ok(ExpansionResult::Expanded { raw_sdl, .. }) => {
                        Supergraph::new_with_router_specs(&raw_sdl).ok().and_then(|sg| {
                            let api = sg.to_api_schema(ApiSchemaOptions::default()).ok()?;
                            build_federated_query_graph(sg.schema.clone(), api, Some(false), Some(true)).ok()
                        })
                    }
                    _ => None,
                };

                eprintln!(
                    "DISTANCE {name}: source_aware_edges={my_edges:?}  raw_graph=[{}]  expanded_graph=[{}]",
                    raw_graph.as_ref().map(|g| format!("nodes={} edges={} key={}", g.graph().node_count(), g.graph().edge_count(), key_edges(g))).unwrap_or_else(|| "BUILD FAILED".into()),
                    expanded_graph.as_ref().map(|g| format!("nodes={} edges={} key={}", g.graph().node_count(), g.graph().edge_count(), key_edges(g))).unwrap_or_else(|| "BUILD FAILED".into()),
                );
            });
        });
    }

    /// The supergraph-level collector gathers a `KeyResolution` edge with a
    /// non-empty condition for every keyed connector across all subgraphs of a
    /// (non-expanded) supergraph.
    #[test]
    fn build_connector_source_edges_collects_keyed_connectors() {
        let mut total = 0usize;

        insta::with_settings!({prepend_module_to_snapshot => false}, {
            glob!("../connectors/expand/tests/schemas/expand", "*.graphql", |path| {
                let sdl = std::fs::read_to_string(path).unwrap();
                let Ok(supergraph) =
                    FederationSchema::new(Schema::parse(&sdl, "supergraph.graphql").unwrap())
                else {
                    return;
                };
                // Skip fixtures that don't extract into subgraphs cleanly.
                let Ok(edges) = build_connector_source_edges(&supergraph) else {
                    return;
                };

                for edge in &edges {
                    assert_eq!(
                        edge.transition,
                        QueryGraphEdgeTransition::KeyResolution,
                        "non-KeyResolution connector edge in {path:?}"
                    );
                    let conditions = edge
                        .conditions
                        .as_ref()
                        .expect("a source-entering edge always carries a condition");
                    assert!(!conditions.is_empty(), "empty condition in {path:?}");
                }
                total += edges.len();
            });
        });

        assert!(
            total > 0,
            "no connector source edges collected across fixtures"
        );
    }

    /// `QueryGraph::has_connector_boundary_copies` really separates the two
    /// paths, so checks that gate on it are neither dead nor firing everywhere.
    ///
    /// It is the graph-wide form of the "same type ⇒ same fields reachable"
    /// question, used where no node is available to ask
    /// `reaches_connector_boundary_copy` about — currently the non-empty-residual
    /// warning in `fetch_dependency_graph::is_node_unneeded`. That check can only
    /// ever be reached on the source-aware side, and this is what says so:
    /// `true` on a raw graph the restrictive-provides pass mutated, `false` on
    /// the same schema's expanded graph, where no copy exists by construction.
    #[test]
    fn boundary_copy_presence_separates_source_aware_from_expansion() {
        use crate::ApiSchemaOptions;
        use crate::Supergraph;
        use crate::connectors::expand::ExpansionResult;
        use crate::connectors::expand::expand_connectors;
        use crate::query_graph::build_query_graph::build_federated_query_graph;

        // steelthread restricts (`Query.users` selects `id name`, so `username`
        // is pruned from its landing copy), which is what makes it the fixture
        // that can distinguish the two graphs at all.
        let sdl = include_str!("../connectors/expand/tests/schemas/expand/steelthread.graphql");

        let build = |sdl: &str| {
            let supergraph = Supergraph::new_with_router_specs(sdl).unwrap();
            let api = supergraph
                .to_api_schema(ApiSchemaOptions::default())
                .unwrap();
            build_federated_query_graph(supergraph.schema.clone(), api, Some(false), Some(true))
                .unwrap()
        };

        // Raw graph, before the pass: no copies yet.
        let mut raw = build(sdl);
        assert!(
            !raw.has_connector_boundary_copies(),
            "a freshly built graph has no boundary copies until the pass runs"
        );

        // Raw graph, after the pass: copies exist.
        let fed = FederationSchema::new(Schema::parse(sdl, "supergraph.graphql").unwrap()).unwrap();
        let connectors: Vec<Connector> = extract_subgraphs_from_supergraph(&fed, Some(false))
            .unwrap()
            .subgraphs
            .values()
            .flat_map(|sg| Connector::from_schema(sg.schema.schema(), &sg.name).unwrap_or_default())
            .collect();
        assert!(!connectors.is_empty(), "steelthread has connectors");
        assert!(
            restrict_connector_reachability(&mut raw, &connectors).unwrap(),
            "steelthread has prunable fields, so the pass must mutate the graph"
        );
        assert!(
            raw.has_connector_boundary_copies(),
            "after the restrictive-provides pass the raw graph has boundary copies"
        );

        // Expanded graph, same schema: the pass has nothing to act on, and the
        // predicate must stay false so expansion-path checks never fire.
        let expanded_sdl = match expand_connectors(
            sdl,
            &ApiSchemaOptions {
                include_defer: true,
                ..Default::default()
            },
        ) {
            Ok(ExpansionResult::Expanded { raw_sdl, .. }) => raw_sdl,
            Ok(ExpansionResult::Unchanged) => panic!("steelthread must expand"),
            Err(e) => panic!("steelthread expansion failed: {e}"),
        };
        let mut expanded = build(&expanded_sdl);
        assert!(
            !expanded.has_connector_boundary_copies(),
            "an expanded graph has no boundary copies"
        );
        assert!(
            !restrict_connector_reachability(&mut expanded, &connectors).unwrap(),
            "the pass finds nothing to restrict on an expanded graph"
        );
        assert!(
            !expanded.has_connector_boundary_copies(),
            "and so leaves the expanded graph without copies"
        );
    }
}
