//! @context / @fromContext helpers for the BULB planner: classify how a
//! field with `required_contexts` is isolated (the isolating hop itself is a
//! generic self-key-hop [`super::routing::RoutingChoice`], committed by the
//! ordinary key-hop path), add context selections to the parent fetch, build
//! context rewrite paths, and splice `$contextualArgument_N_M` variables
//! into field arguments.
//!
//! ## Why context selections are direct appends, not condition pendings
//!
//! Context selections look like requirements ("context data must exist
//! before the isolated fetch runs"), and every other requirement in this
//! planner routes through the pending stack as a condition selection. They
//! can't, because their anchor is different in kind: a condition pending is
//! anchored at the pending's own position -- `fork()` keeps the pending's
//! `query_graph_node`, and the condition fields are selected alongside the
//! field that needs them. A context selection instead belongs at an
//! ancestor position: its op path is a strict prefix of the pending's op
//! path (`op_path_prefix(.., ancestor_idx)`), and its fields are defined on
//! the ancestor's type, not the pending's. Routing a selection requires the
//! query-graph node at its anchor, and the pending stack does not retain
//! ancestor query-graph nodes -- `parent_types` keeps only the supergraph
//! type positions needed for the backward @context walk. Anchoring the fork
//! at the pending's node would enumerate options for the ancestor's fields
//! on the wrong type entirely. Pendings do not currently carry their
//! ancestor query-graph spine, so the append to the parent fetch at the
//! ancestor prefix stays direct.
//!
//! The one exception is an ancestor inside the pending's own fetch whose
//! subgraph cannot resolve the context fields (e.g. they are @external
//! there). Its query-graph node is recovered by type and subgraph, and the
//! fields route as condition pendings anchored there.

use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::executable::Argument;
use apollo_compiler::executable::Value;
use petgraph::graph::NodeIndex;

use super::super::shared_path::SharedPath;
use super::FieldRoutingSearchSpace;
use super::PlanState;
use super::commit::CommitCtx;
use crate::error::FederationError;
use crate::operation::Selection;
use crate::operation::SelectionMap;
use crate::operation::SelectionSet;
use crate::query_graph::ContextCondition;
use crate::query_graph::QueryGraphNodeType;
use crate::query_graph::graph_path::operation::OpPathElement;
use crate::query_plan::FetchDataKeyRenamer;
use crate::query_plan::FetchDataPathElement;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;

/// Parse a @fromContext selection string (e.g. `" { prop }"`) into a
/// `SelectionSet` using the supergraph schema and parent type. Type
/// conditions that do not intersect the parent type are kept rather than
/// rejected: the context selection string may contain fragments for
/// multiple runtime types (e.g. `$ctx ... on A { prop } ... on B { prop }`),
/// and `selections_for_runtime_type` filters to the relevant one later.
pub(super) fn parse_context_selection(
    schema: &ValidFederationSchema,
    parent_type: &CompositeTypeDefinitionPosition,
    selection_str: &str,
) -> Result<SelectionSet, FederationError> {
    let field_value = crate::schema::field_set::parse_field_value_without_validation(
        schema,
        parent_type.type_name().clone(),
        selection_str,
    )?;
    // Skip FieldSet::validate() which rejects type conditions that do not
    // intersect the concrete parent type. The conditions are valid at the
    // abstract level and get filtered per runtime type downstream.
    let fragments = Default::default();
    let selection_set =
        SelectionSet::from_selection_set(&field_value.selection_set, &fragments, schema, &|| {
            Ok(())
        })?;
    Ok(selection_set)
}

/// Drop top-level fragments whose type condition shares no runtime type with
/// `ancestor_type`. The append site rebases the selection onto that type,
/// which rejects such fragments at plan build.
fn applicable_context_selection(
    schema: &ValidFederationSchema,
    selection_set: &SelectionSet,
    ancestor_type: &CompositeTypeDefinitionPosition,
) -> Result<SelectionSet, FederationError> {
    let ancestor_runtime_types = schema.possible_runtime_types(ancestor_type.clone())?;
    let mut kept = SelectionMap::new();
    for selection in selection_set.selections.values() {
        if let Selection::InlineFragment(frag_sel) = selection
            && let Some(tc) = &frag_sel.inline_fragment.type_condition_position
            && schema
                .possible_runtime_types(tc.clone())?
                .is_disjoint(&ancestor_runtime_types)
        {
            continue;
        }
        kept.insert(selection.clone());
    }
    Ok(SelectionSet {
        schema: selection_set.schema.clone(),
        type_position: selection_set.type_position.clone(),
        selections: Arc::new(kept),
    })
}

/// The inner selections of a context selection set relevant to one runtime
/// type: fragments matching `type_name` are unwrapped (the typename fanout
/// already adds the `TypenameEquals` guard); field selections apply to all
/// types and are kept as-is.
fn selections_for_runtime_type(selection_set: &SelectionSet, type_name: &Name) -> SelectionSet {
    let mut merged = SelectionMap::new();
    for selection in selection_set.selections.values() {
        match selection {
            Selection::Field(_) => {
                merged.insert(selection.clone());
            }
            Selection::InlineFragment(frag_sel) => {
                let matches = match &frag_sel.inline_fragment.type_condition_position {
                    Some(tc) => tc.type_name() == type_name,
                    None => true,
                };
                if matches {
                    for inner_sel in frag_sel.selection_set.selections.values() {
                        merged.insert(inner_sel.clone());
                    }
                }
            }
        }
    }
    SelectionSet {
        schema: selection_set.schema.clone(),
        type_position: selection_set.type_position.clone(),
        selections: Arc::new(merged),
    }
}

/// Build context rewrite path elements from a parsed selection set: each
/// field becomes a `Key`, each fragment type condition a `TypenameEquals`.
pub(super) fn build_rewrite_path_from_selections(
    selection_set: &SelectionSet,
    path: &mut Vec<FetchDataPathElement>,
) {
    for selection in selection_set.selections.values() {
        match selection {
            Selection::Field(field_sel) => {
                let field_name = field_sel.field.field_position.field_name().clone();
                path.push(FetchDataPathElement::Key(field_name, Default::default()));
                if let Some(sub) = field_sel.selection_set.as_ref() {
                    build_rewrite_path_from_selections(sub, path);
                }
            }
            Selection::InlineFragment(frag_sel) => {
                if let Some(tc) = &frag_sel.inline_fragment.type_condition_position {
                    path.push(FetchDataPathElement::TypenameEquals(tc.type_name().clone()));
                }
                build_rewrite_path_from_selections(&frag_sel.selection_set, path);
            }
        }
    }
}

/// How a @fromContext field is isolated from siblings that would see
/// different context values.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ContextPlacement {
    /// The field arrived via a key hop: the existing entity fetch provides
    /// the isolation; context selections go on the parent fetch feeding it.
    EntityRoot,
    /// The @context ancestor sits at the entity boundary: the context data
    /// already rides the entity representation; context selections go on
    /// the fetch feeding this entity fetch.
    EntityBoundary,
    /// Same-subgraph field that would need an isolating hop, but routing
    /// offered none (no locally satisfiable @key): the field stays where
    /// it is, without context arguments.
    Unisolated,
}

/// Whether a @fromContext field at this pending's position needs an
/// isolating same-subgraph entity hop: true unless the entity boundary
/// already isolates it (every condition's @context sits at or above the
/// boundary type). Evaluated at routing-enumeration time; when true, option
/// enumeration forces a self key hop and the generic key-hop commit creates
/// the isolation.
pub(super) fn needs_context_isolation(
    pending: &super::PendingSelection,
    required_contexts: &[ContextCondition],
) -> bool {
    !at_entity_boundary(pending, required_contexts)
}

/// Handle @fromContext: resolve context conditions from ancestor types,
/// pass context values through the isolating entity fetch's inputs, and add
/// context rewrites/variables. The isolating hop itself was created by the
/// generic key-hop commit (the routing choice was a self key hop or a
/// cross-subgraph hop); this only does the context-specific parts. Returns
/// the fetch node and op_path for the field.
pub(super) fn handle_from_context(
    search_space: &FieldRoutingSearchSpace,
    state: &mut PlanState,
    ctx: &CommitCtx<'_>,
    current_fetch_node: NodeIndex,
    child_op_path: &SharedPath<Arc<OpPathElement>>,
    required_contexts: &[ContextCondition],
) -> Result<(NodeIndex, SharedPath<Arc<OpPathElement>>), FederationError> {
    let pending = ctx.pending;
    // The field edge's source gives the subgraph defining the @fromContext
    // field; for key hops this differs from pending.query_graph_node.
    let (edge_source_node, _) = search_space.qg().edge_endpoints(
        ctx.choice.edge_index().ok_or_else(|| {
            FederationError::internal("edge_index called on non-edge routing choice")
        })?,
    )?;
    let edge_source_data = search_space.qg()
        .node_weight(edge_source_node)?;
    let source_subgraph = &edge_source_data.source;

    let placement = classify_placement(pending, current_fetch_node, required_contexts);
    let (context_fetch_node, parent_fetch_node) = match placement {
        ContextPlacement::EntityRoot => (current_fetch_node, pending.fetch_node),
        ContextPlacement::EntityBoundary => {
            (current_fetch_node, pending.context_anchor.fetch.unwrap())
        }
        // No isolation was possible: leave the field where it is.
        ContextPlacement::Unisolated => return Ok((current_fetch_node, child_op_path.clone())),
    };

    // Process each context condition, collecting (argument, context_id)
    // pairs to splice into the field's arguments.
    let mut context_args: Vec<(Name, Name)> = Vec::new();
    for cond in required_contexts {
        let context_id = search_space.qg()
            .context_id_by_source_and_argument(source_subgraph, &cond.argument_coordinate)?;

        let (ancestor_type, ancestor_idx, levels_in_data_path) =
            find_context_ancestor(pending, cond)?;

        // An ancestor at the entity boundary rides the entity
        // representation: no Parent elements needed, use the parent fetch's
        // op_path.
        let (append_fetch_node, ancestor_op_path, levels_in_data_path) =
            if placement == ContextPlacement::EntityBoundary {
                (parent_fetch_node, pending.context_anchor.op_path.clone(), 0)
            } else {
                locate_ancestor_site(
                    state,
                    pending,
                    parent_fetch_node,
                    &ancestor_type,
                    ancestor_idx,
                    levels_in_data_path,
                )?
            };

        let context_selection = applicable_context_selection(
            &search_space.supergraph_schema,
            &parse_context_selection(
                &search_space.supergraph_schema,
                &ancestor_type,
                &cond.selection,
            )?,
            &ancestor_type,
        )?;

        // Verify that the subgraph receiving this context selection can
        // actually resolve the fields. Context selections are parsed against
        // the supergraph schema (where every field exists), but the target
        // subgraph may declare them as @external.
        let append_subgraph = state.graph.node(append_fetch_node).subgraph.clone();
        let append_schema = search_space
            .cached_query_graph.query_graph
            .schema_by_source(&append_subgraph)?;
        let append_type: CompositeTypeDefinitionPosition = append_schema
            .get_type(ancestor_type.type_name())?
            .try_into()?;
        if super::conditions::can_satisfy_conditions(
            &context_selection,
            &append_type,
            append_schema,
        ) {
            state.graph.append_selection(
                append_fetch_node,
                &ancestor_op_path,
                Some(&Arc::new(context_selection.clone())),
            );
        } else {
            // Route the context fields like any other requirement instead,
            // so a key hop can fetch them from a subgraph that resolves them.
            let local_site = placement != ContextPlacement::EntityBoundary
                && append_fetch_node == parent_fetch_node;
            let anchor = local_site
                .then(|| {
                    ancestor_anchor(
                        search_space,
                        pending,
                        &append_subgraph,
                        &ancestor_type,
                        ancestor_idx,
                        &ancestor_op_path,
                    )
                })
                .flatten()
                .ok_or_else(|| {
                    FederationError::internal(format!(
                        "@fromContext field {} requires context data that subgraph `{}` cannot resolve (fields may be @external)",
                        cond.argument_coordinate, append_subgraph,
                    ))
                })?;
            search_space.push_condition_pendings(
                state,
                &anchor,
                &Arc::new(context_selection.clone()),
                context_fetch_node,
            )?;
        }

        add_context_renamers(
            search_space,
            state,
            context_fetch_node,
            &ancestor_type,
            &context_selection,
            levels_in_data_path,
            context_id,
            cond,
        )?;

        context_args.push((cond.argument_name.clone(), context_id.clone()));
    }

    // The entity fetch already has the right op_path structure: just add
    // context arguments to the field element.
    let final_op_path = match child_op_path.last() {
        Some(last) => pushed_with_context_args(child_op_path.parent(), last, &context_args),
        None => child_op_path.clone(),
    };
    Ok((context_fetch_node, final_op_path))
}

/// See [`ContextPlacement`].
fn classify_placement(
    pending: &super::PendingSelection,
    current_fetch_node: NodeIndex,
    required_contexts: &[ContextCondition],
) -> ContextPlacement {
    if current_fetch_node != pending.fetch_node {
        ContextPlacement::EntityRoot
    } else if at_entity_boundary(pending, required_contexts) {
        ContextPlacement::EntityBoundary
    } else {
        ContextPlacement::Unisolated
    }
}

/// Whether every context condition's nearest @context ancestor is the
/// entity boundary type, the one the entity representation carries, so the
/// context data rides the representation and no isolating hop is needed.
fn at_entity_boundary(
    pending: &super::PendingSelection,
    required_contexts: &[ContextCondition],
) -> bool {
    // Same pairing as `locate_ancestor_site`: the boundary type is the one
    // paired with op_path's first element.
    let boundary_idx = pending
        .parent_types
        .len()
        .checked_sub(pending.op_path.len());
    pending.context_anchor.fetch.is_some()
        && boundary_idx.is_some_and(|boundary_idx| {
            required_contexts.iter().all(|cond| {
                find_context_ancestor(pending, cond)
                    .is_ok_and(|(_, ancestor_idx, _)| ancestor_idx == boundary_idx)
            })
        })
}

/// Walk backward through `parent_types` to the nearest ancestor carrying the
/// condition's @context. Returns the ancestor type, its index into the
/// parent-types stack, and how many data-path levels (Field elements only;
/// inline fragments add no level) separate the pending from it.
///
/// `parent_types = [T0..Tn]` has one more element than
/// `op_path = [E0..En-1]` (T0, the root type, has no incoming transition;
/// E_i is the transition T_i to T_{i+1}).
fn find_context_ancestor(
    pending: &super::PendingSelection,
    cond: &ContextCondition,
) -> Result<(CompositeTypeDefinitionPosition, usize, usize), FederationError> {
    let parent_types_vec = pending.parent_types.to_vec();
    let op_path_vec: Vec<_> = pending.op_path.iter().collect();
    let mut levels_in_data_path = 0usize;
    // Walk backward from the declaring type to ancestors. The first entry
    // (i=0) is the declaring type itself; skip the @context match for it
    // so the field reads context from an actual ancestor, not its own type.
    for (i, ancestor_type) in parent_types_vec.iter().rev().enumerate() {
        if i > 0 && cond.types_with_context_set.contains(ancestor_type) {
            let ancestor_idx = parent_types_vec.len() - 1 - i;
            return Ok((ancestor_type.clone(), ancestor_idx, levels_in_data_path));
        }
        // The transition traversed backward is op_path[len - 1 - i]. Guard
        // against underflow: parent_types has one more element than op_path
        // (the root type can be the last candidate).
        if let Some(op_element) = op_path_vec
            .len()
            .checked_sub(1 + i)
            .and_then(|idx| op_path_vec.get(idx))
            && matches!(op_element.as_ref(), OpPathElement::Field(_))
        {
            levels_in_data_path += 1;
        }
    }
    Err(FederationError::internal(format!(
        "@fromContext argument {} has no ancestor type with the required @context set",
        cond.argument_coordinate,
    )))
}

/// Where a @fromContext ancestor's context selection must be recorded: the
/// fetch group, op-path prefix, and Parent-level count at which the
/// ancestor type's data is fetched.
///
/// `parent_types` accumulates across entity hops while `op_path` restarts
/// at each hop (its first element pairs with the boundary type), so an
/// ancestor index maps onto `op_path` only when the ancestor lies within
/// the current fetch. An ancestor above the boundary lives in the anchor
/// fetch, at a prefix of the anchor op path. Anything further up (two or
/// more hops) has no recorded spine; the commit fails cleanly so the
/// search backs out instead of recording a selection that cannot rebase at
/// plan build.
fn locate_ancestor_site(
    state: &PlanState,
    pending: &super::PendingSelection,
    local_fetch: NodeIndex,
    ancestor_type: &CompositeTypeDefinitionPosition,
    ancestor_idx: usize,
    local_levels: usize,
) -> Result<(NodeIndex, SharedPath<Arc<OpPathElement>>, usize), FederationError> {
    let op_len = pending.op_path.len() as isize;
    let types_len = pending.parent_types.len() as isize;
    // op_path's last element pairs with the last parent type; walking the
    // pairing backward puts the ancestor at this prefix length.
    let local_prefix = op_len + 1 - (types_len - ancestor_idx as isize);
    let in_entity_fetch = pending.context_anchor.fetch.is_some();
    if local_prefix > 0 || (local_prefix == 0 && !in_entity_fetch) {
        let prefix = op_path_prefix(&pending.op_path, local_prefix as usize);
        verify_prefix_lands_on(state, local_fetch, &prefix, ancestor_type)?;
        return Ok((local_fetch, prefix, local_levels));
    }
    let anchor = &pending.context_anchor;
    let Some(anchor_fetch) = anchor.fetch else {
        return Err(FederationError::internal(format!(
            "@fromContext ancestor {} lies outside the current fetch group with no anchor",
            ancestor_type.type_name(),
        )));
    };
    // Index of the boundary type, the one paired with op_path's first
    // element; the anchor op path's last element pairs with it too.
    let boundary_idx = types_len - op_len;
    let anchor_prefix = anchor.op_path.len() as isize - (boundary_idx - ancestor_idx as isize);
    if anchor_prefix < 0 {
        return Err(FederationError::internal(format!(
            "@fromContext ancestor {} lies more than one fetch group above its field",
            ancestor_type.type_name(),
        )));
    }
    let prefix = op_path_prefix(&anchor.op_path, anchor_prefix as usize);
    verify_prefix_lands_on(state, anchor_fetch, &prefix, ancestor_type)?;
    // Parent levels span the whole data path back to the ancestor: every
    // field in the current fetch's op path, plus the anchor-fetch fields
    // between the ancestor and the boundary.
    let levels = count_field_elements(pending.op_path.iter())
        + count_field_elements(anchor.op_path.iter().skip(anchor_prefix as usize));
    Ok((anchor_fetch, prefix, levels))
}

/// A pending positioned at a @fromContext ancestor within the pending's own
/// fetch, used to route context fields the fetch's subgraph cannot resolve.
/// `None` when the ancestor has no plain query graph node in that subgraph.
fn ancestor_anchor(
    search_space: &FieldRoutingSearchSpace,
    pending: &super::PendingSelection,
    subgraph: &Arc<str>,
    ancestor_type: &CompositeTypeDefinitionPosition,
    ancestor_idx: usize,
    ancestor_op_path: &SharedPath<Arc<OpPathElement>>,
) -> Option<super::PendingSelection> {
    let graph = search_space.qg().graph();
    let node = graph.node_indices().find(|&idx| {
        let data = &graph[idx];
        data.source == *subgraph
            && data.provide_id.is_none()
            && matches!(&data.type_, QueryGraphNodeType::SchemaType(pos) if pos.type_name() == ancestor_type.type_name())
    })?;
    let parent_types = pending.parent_types.to_vec();
    let fields = count_field_elements(ancestor_op_path.iter());
    let mut anchor = pending
        .clone()
        .at(node, pending.fetch_node)
        .with_op_path(ancestor_op_path.clone())
        .with_response_path(path_in_fetch_prefix(&pending.path_in_fetch, fields))
        .with_provides_anchor(None)
        .with_parent_types(SharedPath::from_vec(
            parent_types.get(..=ancestor_idx)?.to_vec(),
        ))
        .with_context_anchor(Default::default())
        .with_narrowing(Default::default());
    anchor.condition = None;
    Some(anchor)
}

/// The leading elements of `path` that cover its first `fields` fields: each
/// field contributes a `Key` plus any list indexes that follow it.
fn path_in_fetch_prefix(
    path: &SharedPath<FetchDataPathElement>,
    fields: usize,
) -> SharedPath<FetchDataPathElement> {
    let mut seen = 0;
    let mut prefix = SharedPath::new();
    for element in path.iter() {
        if matches!(element, FetchDataPathElement::Key(..)) {
            if seen == fields {
                break;
            }
            seen += 1;
        }
        prefix = prefix.pushed(element.clone());
    }
    prefix
}

/// Guard for [`locate_ancestor_site`]: the computed prefix must land on the
/// ancestor's type, otherwise the appended context selection would fail to
/// rebase at plan build.
fn verify_prefix_lands_on(
    state: &PlanState,
    fetch: NodeIndex,
    prefix: &SharedPath<Arc<OpPathElement>>,
    ancestor_type: &CompositeTypeDefinitionPosition,
) -> Result<(), FederationError> {
    let landing = match prefix.last() {
        Some(element) => element
            .sub_selection_type_position()?
            .map(|t| t.type_name().clone()),
        None => state
            .graph
            .node(fetch)
            .root_type()
            .map(|t| t.type_name().clone()),
    };
    if landing.as_ref() == Some(ancestor_type.type_name()) {
        Ok(())
    } else {
        Err(FederationError::internal(format!(
            "@fromContext ancestor {} does not match its computed fetch position",
            ancestor_type.type_name(),
        )))
    }
}

fn count_field_elements<'a>(iter: impl Iterator<Item = &'a Arc<OpPathElement>>) -> usize {
    iter.filter(|e| matches!(e.as_ref(), OpPathElement::Field(_)))
        .count()
}

/// The first `len` elements of `op_path`, the transitions T0 to T1 up to
/// the ancestor at index `len`.
fn op_path_prefix(
    op_path: &SharedPath<Arc<OpPathElement>>,
    len: usize,
) -> SharedPath<Arc<OpPathElement>> {
    let mut prefix = SharedPath::new();
    for element in op_path.iter().take(len) {
        prefix = prefix.pushed(element.clone());
    }
    prefix
}

/// Emit the context rewrite(s) and variable definition on the context
/// fetch: Parent elements up the data path, then the context field path.
/// With Parent elements and a non-Query ancestor, fan out over all runtime
/// types with a TypenameEquals guard each (mirroring
/// `query_planner.rs::add_context_renamers_for_selection_set`).
#[allow(clippy::too_many_arguments)]
fn add_context_renamers(
    search_space: &FieldRoutingSearchSpace,
    state: &mut PlanState,
    context_fetch_node: NodeIndex,
    ancestor_type: &CompositeTypeDefinitionPosition,
    context_selection: &SelectionSet,
    levels_in_data_path: usize,
    context_id: &Name,
    cond: &ContextCondition,
) -> Result<(), FederationError> {
    let base_rewrite_path: Vec<FetchDataPathElement> =
        std::iter::repeat_n(FetchDataPathElement::Parent, levels_in_data_path).collect();

    let add_renamer = |state: &mut PlanState, rewrite_path: Vec<FetchDataPathElement>| {
        let renamer = FetchDataKeyRenamer {
            path: rewrite_path,
            rename_key_to: context_id.clone(),
        };
        state.graph.add_context(
            context_fetch_node,
            renamer,
            context_id.clone(),
            cond.argument_type.clone(),
        );
    };

    let is_root_type = search_space
        .supergraph_schema
        .schema()
        .root_operation(apollo_compiler::executable::OperationType::Query)
        .is_some_and(|root| root == ancestor_type.type_name());
    let needs_typename_fanout = levels_in_data_path > 0 && !is_root_type;
    if needs_typename_fanout {
        let runtime_types = search_space
            .supergraph_schema
            .possible_runtime_types(ancestor_type.clone())?;
        for runtime_type in runtime_types {
            let mut rewrite_path = base_rewrite_path.clone();
            rewrite_path.push(FetchDataPathElement::TypenameEquals(
                runtime_type.type_name.clone(),
            ));
            let inner = selections_for_runtime_type(context_selection, &runtime_type.type_name);
            build_rewrite_path_from_selections(&inner, &mut rewrite_path);
            add_renamer(state, rewrite_path);
        }
    } else {
        let mut rewrite_path = base_rewrite_path;
        build_rewrite_path_from_selections(context_selection, &mut rewrite_path);
        add_renamer(state, rewrite_path);
    }
    Ok(())
}

/// Push `last` onto `base`, splicing `$contextualArgument` variables into
/// its arguments when it is a field. Non-field elements (and empty
/// context-arg lists) are pushed unchanged.
fn pushed_with_context_args(
    base: SharedPath<Arc<OpPathElement>>,
    last: &Arc<OpPathElement>,
    context_args: &[(Name, Name)],
) -> SharedPath<Arc<OpPathElement>> {
    if context_args.is_empty() {
        return base.pushed(last.clone());
    }
    let OpPathElement::Field(field) = last.as_ref() else {
        return base.pushed(last.clone());
    };
    let mut modified_field = field.clone();
    let extra_args = context_args.iter().map(|(arg_name, ctx_id)| {
        Node::new(Argument {
            name: arg_name.clone(),
            value: Node::new(Value::Variable(ctx_id.clone())),
        })
    });
    modified_field.arguments = modified_field
        .arguments
        .iter()
        .cloned()
        .chain(extra_args)
        .collect();
    base.pushed(Arc::new(OpPathElement::Field(modified_field)))
}
