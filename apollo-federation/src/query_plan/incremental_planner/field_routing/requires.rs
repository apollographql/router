//! @requires commit logic for the BULB planner: alias generation, entity
//! input wiring, and input conflict detection. Which resolution strategy
//! applies (in-place vs. entity re-entry) is decided at routing-enumeration
//! time and named by the routing choice; this module only applies it.

use std::collections::BTreeMap;
use std::sync::Arc;

use apollo_compiler::Name;
use petgraph::graph::EdgeIndex;

use super::super::fetch_graph::InputContribution;
use super::super::shared_path::SharedPath;
use super::FieldRoutingSearchSpace;
use super::commit::CommitCtx;
use super::commit::CommitTarget;
use super::state::PendingSelection;
use super::state::PlanState;
use crate::error::FederationError;
use crate::operation::DirectiveList;
use crate::operation::Field;
use crate::operation::FieldSelection;
use crate::operation::Selection;
use crate::operation::SelectionMap;
use crate::operation::SelectionSet;
use crate::operation::TYPENAME_FIELD;
use crate::query_graph::graph_path::operation::OpPathElement;

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// Whether any top-level condition field carries arguments. Such fields can
/// collide with a user selection of the same field under different arguments
/// (the conflict-alias rename-back would clobber the condition data), so
/// they must be aliased; argument-less ones safely share the user
/// selection's key and stay un-aliased to avoid duplicate fetching.
fn condition_fields_take_arguments(conditions: &SelectionSet) -> bool {
    conditions.selections.values().any(|sel| {
        let Selection::Field(field_sel) = sel else {
            return false;
        };
        if *field_sel.field.name() == TYPENAME_FIELD {
            return false;
        }
        !field_sel.field.arguments.is_empty()
    })
}

/// An aliased condition SelectionSet plus (alias, original_name) pairs for
/// generating input rewrites on the consuming entity fetch.
type AliasedConditions = (Arc<SelectionSet>, Vec<(Name, Name)>);

/// Alias top-level field selections in a @requires condition set so their
/// response data lands under a unique key, avoiding collisions with
/// user-requested fields at the same response path.
///
/// Fields named in `keep_raw` stay un-aliased: they deliberately share the
/// user selection's response key so the condition rides (and dedupes with)
/// the user fetch chain instead of staging an aliased duplicate.
pub(super) fn alias_condition_fields(
    conditions: &Arc<SelectionSet>,
    alias_ids: &mut BTreeMap<String, usize>,
    keep_raw: &[Name],
) -> Result<AliasedConditions, FederationError> {
    let mut alias_rewrites = Vec::new();
    let mut new_map = SelectionMap::new();
    for sel in conditions.selections.values() {
        match sel {
            Selection::Field(field_sel) => {
                if *field_sel.field.name() == TYPENAME_FIELD
                    || keep_raw.contains(field_sel.field.name())
                {
                    new_map.insert(sel.clone());
                    continue;
                }
                let original_name = field_sel.field.response_name().clone();
                // Intern the alias id by serialized selection: identical
                // conditions share an alias (sibling fetches staging the same
                // @requires can merge); conditions differing in arguments or
                // sub-selection get distinct aliases.
                let next_id = alias_ids.len();
                let idx = *alias_ids.entry(sel.to_string()).or_insert(next_id);
                let alias = Name::new(&format!("__require_{idx}_{original_name}"))
                    .map_err(|_| FederationError::internal("invalid condition alias name"))?;
                let aliased_field = Field {
                    alias: Some(alias.clone()),
                    schema: field_sel.field.schema.clone(),
                    field_position: field_sel.field.field_position.clone(),
                    arguments: field_sel.field.arguments.clone(),
                    directives: field_sel.field.directives.clone(),
                    sibling_typename: field_sel.field.sibling_typename.clone(),
                };
                let aliased_sel = Selection::Field(Arc::new(FieldSelection {
                    field: aliased_field,
                    selection_set: field_sel.selection_set.clone(),
                }));
                alias_rewrites.push((alias, original_name));
                new_map.insert(aliased_sel);
            }
            other => {
                new_map.insert(other.clone());
            }
        }
    }
    let aliased_ss = SelectionSet {
        schema: conditions.schema.clone(),
        type_position: conditions.type_position.clone(),
        selections: Arc::new(new_map),
    };
    Ok((Arc::new(aliased_ss), alias_rewrites))
}

/// Widen `conditions` so each top-level field sharing a response name with a
/// plain (un-aliased) input field already riding `node`'s representation for
/// `source_type` also selects that sibling's sub-fields. The alias's
/// rename-back on the consuming fetch is a remove+insert on the response
/// object, so the aliased data replaces whatever the plain sibling put under
/// that key. Carrying the sibling's selection inside the aliased field keeps
/// the post-rename object complete. Fields named in `keep_raw` are exempt:
/// they stay un-aliased, so no rename-back ever removes the sibling's data
/// from under the shared key.
fn widen_conditions_with_plain_sibling_inputs(
    conditions: &Arc<SelectionSet>,
    graph: &super::super::fetch_graph::FetchGraph,
    node: petgraph::graph::NodeIndex,
    source_type: &Name,
    keep_raw: &[Name],
) -> Result<Arc<SelectionSet>, FederationError> {
    let mut widened: Option<SelectionSet> = None;
    for input in graph.incoming_inputs(node) {
        if !input.condition_alias_rewrites.is_empty() || input.source_type_name != *source_type {
            continue;
        }
        for sibling in input.conditions.selections.values() {
            let Selection::Field(sibling_field) = sibling else {
                continue;
            };
            let Some(sibling_sub) = &sibling_field.selection_set else {
                continue;
            };
            let name = sibling_field.field.response_name();
            let collides = !keep_raw.contains(name)
                && conditions.selections.values().any(|sel| {
                    matches!(sel, Selection::Field(f)
                        if f.field.response_name() == name && f.selection_set.is_some())
                });
            if !collides {
                continue;
            }
            let target = widened.get_or_insert_with(|| (**conditions).clone());
            let mut new_map = SelectionMap::new();
            for sel in target.selections.values() {
                if let Selection::Field(f) = sel
                    && f.field.response_name() == name
                    && let Some(sub) = &f.selection_set
                {
                    let mut merged = sub.clone();
                    merged.add_selection_set(sibling_sub)?;
                    new_map.insert(Selection::Field(Arc::new(FieldSelection {
                        field: f.field.clone(),
                        selection_set: Some(merged),
                    })));
                } else {
                    new_map.insert(sel.clone());
                }
            }
            target.selections = Arc::new(new_map);
        }
    }
    Ok(widened.map(Arc::new).unwrap_or_else(|| conditions.clone()))
}

/// The trailing inline-fragment elements of `op_path` (after the last field)
/// that carry directives: @skip/@include conditions at the current position,
/// which a key hop must carry into the entity fetch's op path or the hopped
/// selections lose their gating.
pub(super) fn trailing_condition_fragments(
    op_path: &SharedPath<Arc<OpPathElement>>,
) -> Vec<Arc<OpPathElement>> {
    let mut trailing = Vec::new();
    for element in op_path.iter() {
        match element.as_ref() {
            OpPathElement::Field(_) => trailing.clear(),
            OpPathElement::InlineFragment(frag) => {
                if !frag.directives.is_empty() {
                    trailing.push(element.clone());
                }
            }
        }
    }
    trailing
}

/// `op_path` with @skip/@include stripped from its inline-fragment elements,
/// for appending key and @requires input selections. Inputs must be selected
/// unconditionally: an input gated by one branch's Boolean condition leaves
/// the representation incomplete whenever a different branch executes.
/// Condition-only fragments are dropped; type-conditioned fragments keep the
/// downcast without the conditions.
pub(super) fn unconditioned_input_path(
    op_path: &SharedPath<Arc<OpPathElement>>,
) -> SharedPath<Arc<OpPathElement>> {
    let mut elements = Vec::with_capacity(op_path.len());
    for element in op_path.iter() {
        match element.as_ref() {
            OpPathElement::Field(_) => elements.push(element.clone()),
            OpPathElement::InlineFragment(frag) => {
                let stripped: DirectiveList = frag
                    .directives
                    .iter()
                    .filter(|d| d.name != "skip" && d.name != "include")
                    .cloned()
                    .collect();
                if stripped.len() == frag.directives.len() {
                    elements.push(element.clone());
                } else if !stripped.is_empty() || frag.type_condition_position.is_some() {
                    elements.push(Arc::new(OpPathElement::InlineFragment(
                        frag.with_updated_directives(stripped),
                    )));
                }
            }
        }
    }
    SharedPath::from_vec(elements)
}

/// Whether any element of `path` is gated by @skip/@include.
fn path_has_boolean_conditions(path: &SharedPath<Arc<OpPathElement>>) -> bool {
    path.iter().any(|element| {
        let directives = match element.as_ref() {
            OpPathElement::Field(field) => &field.directives,
            OpPathElement::InlineFragment(frag) => &frag.directives,
        };
        directives
            .iter()
            .any(|d| d.name == "skip" || d.name == "include")
    })
}

/// A user field a @requires condition field named `name` can safely share a
/// response key with: same name, un-aliased, argument-less, and not gated
/// by @skip/@include (inputs must be unconditional).
fn is_shareable_user_field(field: &Field, name: &Name) -> bool {
    field.alias.is_none()
        && field.name() == name
        && field.arguments.is_empty()
        && !field
            .directives
            .iter()
            .any(|d| d.name == "skip" || d.name == "include")
}

/// Structural equality of op-path elements, with an `Arc` fast path (sibling
/// pendings share their parent's path `Arc`s).
fn same_op_element(a: &Arc<OpPathElement>, b: &Arc<OpPathElement>) -> bool {
    if Arc::ptr_eq(a, b) {
        return true;
    }
    match (a.as_ref(), b.as_ref()) {
        (OpPathElement::Field(x), OpPathElement::Field(y)) => {
            x.response_name() == y.response_name()
                && x.name() == y.name()
                && x.arguments == y.arguments
                && x.directives == y.directives
        }
        (OpPathElement::InlineFragment(x), OpPathElement::InlineFragment(y)) => {
            x.type_condition_position == y.type_condition_position && x.directives == y.directives
        }
        _ => false,
    }
}

/// Walk `selections` down the op-path `elements`, matching fields by
/// response name/name/arguments and inline fragments by type condition.
/// `None` when any element has no matching selection.
fn descend_selections<'a>(
    mut selections: &'a SelectionSet,
    elements: &[&Arc<OpPathElement>],
) -> Option<&'a SelectionSet> {
    for element in elements {
        let next = match element.as_ref() {
            OpPathElement::Field(f) => selections.selections.values().find_map(|sel| match sel {
                Selection::Field(fs)
                    if fs.field.response_name() == f.response_name()
                        && fs.field.name() == f.name()
                        && fs.field.arguments == f.arguments =>
                {
                    fs.selection_set.as_ref()
                }
                _ => None,
            }),
            OpPathElement::InlineFragment(fr) => {
                selections.selections.values().find_map(|sel| match sel {
                    Selection::InlineFragment(is)
                        if is.inline_fragment.type_condition_position
                            == fr.type_condition_position =>
                    {
                        Some(&is.selection_set)
                    }
                    _ => None,
                })
            }
        };
        selections = next?;
    }
    Some(selections)
}

/// Whether a fetch group's committed selections include a shareable user
/// selection of `name` at exactly position `anchor`.
fn committed_selects_field(
    entries: &[super::super::fetch_graph::selection_builder::SelectionEntry],
    anchor: &[&Arc<OpPathElement>],
    name: &Name,
) -> bool {
    'entries: for entry in entries {
        let path: Vec<&Arc<OpPathElement>> = entry.path().iter().collect();
        let common = path.len().min(anchor.len());
        if !path[..common]
            .iter()
            .zip(&anchor[..common])
            .all(|(e, a)| same_op_element(e, a))
        {
            continue 'entries;
        }
        if path.len() > anchor.len() {
            if let OpPathElement::Field(field) = path[anchor.len()].as_ref()
                && is_shareable_user_field(field, name)
            {
                return true;
            }
            continue 'entries;
        }
        let Some(root) = entry.selections().map(|arc| arc.as_ref()) else {
            continue 'entries;
        };
        let Some(selections) = descend_selections(root, &anchor[path.len()..]) else {
            continue 'entries;
        };
        if selections.selections.values().any(
            |sel| matches!(sel, Selection::Field(fs) if is_shareable_user_field(&fs.field, name)),
        ) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Methods on FieldRoutingSearchSpace
// ---------------------------------------------------------------------------

impl FieldRoutingSearchSpace {
    /// Top-level @requires condition fields that can reuse the user
    /// selection's response key instead of staging an aliased duplicate.
    fn shareable_condition_fields(
        &self,
        state: &PlanState,
        pending: &PendingSelection,
        source: &super::NodeSource,
        conditions: &SelectionSet,
    ) -> Vec<Name> {
        use super::super::fetch_graph::FetchGroupKind;

        if path_has_boolean_conditions(&pending.op_path) {
            return Vec::new();
        }
        let anchor: Vec<&Arc<OpPathElement>> = pending.op_path.iter().collect();
        let sibling_scan: Option<(Vec<crate::query_plan::FetchDataPathElement>, _)> = self
            .entity_root_path(source.type_pos.type_name())
            .ok()
            .map(|root| (self.pending_merge_at(state, pending), root));
        let mut shared = Vec::new();
        for sel in conditions.selections.values() {
            let Selection::Field(cond_field) = sel else {
                continue;
            };
            let name = cond_field.field.name();
            if *name == TYPENAME_FIELD || !cond_field.field.arguments.is_empty() {
                continue;
            }
            let on_stack = state.pending.iter().any(|p| {
                p.fetch_node == pending.fetch_node
                    && p.condition.is_none()
                    && matches!(
                        &p.selection,
                        Selection::Field(user) if is_shareable_user_field(&user.field, name)
                    )
                    && p.op_path.len() == anchor.len()
                    && p.op_path
                        .iter()
                        .zip(anchor.iter())
                        .all(|(a, b)| same_op_element(a, b))
            });
            let committed = on_stack
                || committed_selects_field(
                    state
                        .graph
                        .node(pending.fetch_node)
                        .selection_builder
                        .entries(),
                    &anchor,
                    name,
                );
            let in_sibling_entity = committed
                || sibling_scan.as_ref().is_some_and(|(merge_at, root)| {
                    let root_anchor: Vec<&Arc<OpPathElement>> = root.iter().collect();
                    state.graph.node_indices().any(|idx| {
                        idx != pending.fetch_node
                            && matches!(state.graph.node(idx).kind, FetchGroupKind::Entity { .. })
                            && state.graph.merge_at(idx) == *merge_at
                            && committed_selects_field(
                                state.graph.node(idx).selection_builder.entries(),
                                &root_anchor,
                                name,
                            )
                    })
                });
            if in_sibling_entity {
                shared.push(name.clone());
            }
        }
        shared
    }

    /// Conditions staged for a key hop that needs aliasing. Shared fields
    /// stay un-aliased; the remaining (aliased) fields are widened against
    /// plain sibling inputs in the same representation.
    fn stage_aliased_conditions(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
        source: &super::NodeSource,
        requires_conditions: &Arc<SelectionSet>,
        target_fetch_node: petgraph::graph::NodeIndex,
        resolvable_in_place: bool,
    ) -> Result<AliasedConditions, FederationError> {
        let shared = if resolvable_in_place {
            Vec::new()
        } else {
            self.shareable_condition_fields(state, pending, source, requires_conditions)
        };
        let widened = widen_conditions_with_plain_sibling_inputs(
            requires_conditions,
            &state.graph,
            target_fetch_node,
            source.type_pos.type_name(),
            &shared,
        )?;
        alias_condition_fields(&widened, &mut state.condition_alias_ids, &shared)
    }

    /// Handle @requires on the routed edge, applying the resolution strategy
    /// the routing choice already names.
    pub(super) fn apply_requires(
        &self,
        state: &mut PlanState,
        ctx: &CommitCtx<'_>,
        requires_conditions: &Arc<SelectionSet>,
        target: CommitTarget,
    ) -> Result<CommitTarget, FederationError> {
        let CommitCtx {
            pending,
            choice,
            key_hop_edge,
        } = *ctx;

        if choice.hop_kind == super::routing::HopKind::KeyHop {
            let source = self.node_source(pending.query_graph_node)?;
            let resolvable_in_place = choice.requires_resolvable_in_place;

            let has_input_conflict = resolvable_in_place
                && key_hop_edge.is_some_and(|e| {
                    self.has_conflicting_requires_inputs(&state.graph, e, requires_conditions)
                });
            let needs_alias = !resolvable_in_place
                || has_input_conflict
                || condition_fields_take_arguments(requires_conditions);
            let (conditions, alias_rewrites) = if needs_alias {
                self.stage_aliased_conditions(
                    state,
                    pending,
                    &source,
                    requires_conditions,
                    target.fetch_node,
                    resolvable_in_place,
                )?
            } else {
                (requires_conditions.clone(), Vec::new())
            };

            let rewrites_collide = key_hop_edge.is_some_and(|e| {
                state
                    .graph
                    .has_conflicting_condition_rewrites(e, &alias_rewrites)
            });
            let input = InputContribution {
                source_type_name: source.type_pos.type_name().clone(),
                conditions: conditions.clone(),
                rewrite_info: None,
                condition_alias_rewrites: alias_rewrites,
            };
            let dest = if has_input_conflict || rewrites_collide {
                let merge_at = state.graph.merge_at(target.fetch_node).to_vec();
                let split_group = state
                    .graph
                    .add_entity_group(choice.target_subgraph(), merge_at);
                let mut inputs = state.graph.clone_key_inputs(key_hop_edge.unwrap());
                inputs.push(input);
                state
                    .graph
                    .add_dependency(pending.fetch_node, split_group, inputs);
                split_group
            } else {
                if let Some(edge_idx) = key_hop_edge {
                    state.graph.add_input_to_edge(edge_idx, input);
                }
                target.fetch_node
            };

            if resolvable_in_place {
                state.graph.append_selection(
                    pending.fetch_node,
                    &unconditioned_input_path(&pending.op_path),
                    Some(&conditions),
                );
                state
                    .graph
                    .add_ordering_dependency(pending.fetch_node, dest)?;
            } else {
                self.push_condition_pendings(state, pending, &conditions, dest)?;
            }
            return Ok(CommitTarget {
                fetch_node: dest,
                ..target
            });
        }

        if !choice.requires_resolvable_in_place {
            return Err(FederationError::internal(
                "non-hop routing choice with @requires conditions not resolvable in place",
            ));
        }

        // In place: the fields sit alongside the field.
        state.graph.append_selection(
            target.fetch_node,
            &unconditioned_input_path(&pending.op_path),
            Some(requires_conditions),
        );
        Ok(target)
    }

    /// Whether adding `new_conditions` to `edge` would collide in the entity
    /// representation: two conditions sharing a response name but differing
    /// in arguments.
    fn has_conflicting_requires_inputs(
        &self,
        graph: &super::super::fetch_graph::FetchGraph,
        edge: EdgeIndex,
        new_conditions: &Arc<SelectionSet>,
    ) -> bool {
        let existing = &graph.edge_weight_raw(edge).inputs;
        for input in existing {
            if input.rewrite_info.is_some() {
                continue;
            }
            for existing_sel in input.conditions.selections.values() {
                let Selection::Field(existing_field) = existing_sel else {
                    continue;
                };
                let existing_name = existing_field.field.response_name();
                for new_sel in new_conditions.selections.values() {
                    let Selection::Field(new_field) = new_sel else {
                        continue;
                    };
                    if new_field.field.response_name() == existing_name
                        && new_field.field.arguments != existing_field.field.arguments
                    {
                        return true;
                    }
                }
            }
        }
        false
    }
}
