//! @requires commit logic for the BULB planner: alias generation, entity
//! input wiring, and input conflict detection. Which resolution strategy
//! applies (in-place vs. entity re-entry) is decided at routing-enumeration
//! time and named by the routing choice; this module only applies it.

use std::sync::Arc;

use apollo_compiler::Name;
use petgraph::graph::EdgeIndex;

use super::super::fetch_graph::InputContribution;
use super::super::shared_path::SharedPath;
use super::FieldRoutingSearchSpace;
use super::commit::CommitCtx;
use super::commit::CommitTarget;
use super::response_keys::AliasIdentity;
use super::state::PendingSelection;
use super::state::PlanState;
use crate::error::FederationError;
use crate::operation::DirectiveList;
use crate::operation::Field;
use crate::operation::FieldSelection;
use crate::operation::InlineFragment;
use crate::operation::Selection;
use crate::operation::SelectionId;
use crate::operation::SelectionMap;
use crate::operation::SelectionSet;
use crate::query_graph::graph_path::operation::OpPathElement;
use crate::schema::position::CompositeTypeDefinitionPosition;

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// An aliased condition SelectionSet plus (alias, original_name) pairs for
/// generating input rewrites on the consuming entity fetch.
type AliasedConditions = (Arc<SelectionSet>, Vec<(Name, Name)>);

/// Alias the top-level condition fields `needs_alias` picks, so their data
/// lands under a key no other selection uses. The rest keep their own name
/// and merge with any selection of the same field.
///
/// Fields with the same [`AliasIdentity`] share an alias, so conditions on
/// the same field stage one fetch chain between them.
pub(super) fn alias_condition_fields(
    conditions: &Arc<SelectionSet>,
    alias_ids: &mut Vec<AliasIdentity>,
    needs_alias: impl Fn(&FieldSelection) -> bool,
) -> Result<AliasedConditions, FederationError> {
    let mut alias_rewrites = Vec::new();
    let mut new_map = SelectionMap::new();
    for sel in conditions.selections.values() {
        let Selection::Field(field_sel) = sel else {
            new_map.insert(sel.clone());
            continue;
        };
        if !needs_alias(field_sel) {
            new_map.insert(sel.clone());
            continue;
        }
        let identity = AliasIdentity::of(field_sel);
        let idx = match alias_ids.iter().position(|known| *known == identity) {
            Some(i) => i,
            None => {
                alias_ids.push(identity);
                alias_ids.len() - 1
            }
        };
        let original_name = field_sel.field.response_name().clone();
        let alias = Name::new(&format!("__require_{idx}_{original_name}"))
            .map_err(|_| FederationError::internal("invalid condition alias name"))?;
        let aliased_sel = Selection::Field(Arc::new(FieldSelection {
            field: Field {
                alias: Some(alias.clone()),
                ..field_sel.field.clone()
            },
            selection_set: field_sel.selection_set.clone(),
        }));
        alias_rewrites.push((alias, original_name));
        new_map.insert(aliased_sel);
    }
    let aliased_ss = SelectionSet {
        schema: conditions.schema.clone(),
        type_position: conditions.type_position.clone(),
        selections: Arc::new(new_map),
    };
    Ok((Arc::new(aliased_ss), alias_rewrites))
}

/// Widen each aliased condition field whose original name is also a field
/// of a key input on `edge` with that key field's sub-selection. The alias is
/// renamed back by removing and reinserting the key, so without it the rename
/// would drop the key's nested fields from the representation.
fn widen_aliases_with_key_inputs(
    conditions: &Arc<SelectionSet>,
    alias_rewrites: &[(Name, Name)],
    graph: &super::super::fetch_graph::FetchGraph,
    edge: EdgeIndex,
    source_type: &Name,
) -> Result<Arc<SelectionSet>, FederationError> {
    let mut widened: Option<SelectionSet> = None;
    for input in &graph.edge_weight_raw(edge).inputs {
        if input.rewrite_info().is_none() || input.source_type_name() != source_type {
            continue;
        }
        for key_sel in input.conditions().selections.values() {
            let Selection::Field(key_field) = key_sel else {
                continue;
            };
            let Some(key_sub) = &key_field.selection_set else {
                continue;
            };
            let Some((alias, _)) = alias_rewrites
                .iter()
                .find(|(_, original)| original == key_field.field.response_name())
            else {
                continue;
            };
            let target = widened.get_or_insert_with(|| (**conditions).clone());
            let mut new_map = SelectionMap::new();
            for sel in target.selections.values() {
                if let Selection::Field(f) = sel
                    && f.field.response_name() == alias
                    && let Some(sub) = &f.selection_set
                {
                    let mut merged = sub.clone();
                    merged.add_selection_set(key_sub)?;
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

/// The @skip/@include directives of `directives`.
pub(super) fn condition_directives(directives: &DirectiveList) -> DirectiveList {
    directives
        .iter()
        .filter(|d| d.name == "skip" || d.name == "include")
        .cloned()
        .collect()
}

/// The @skip/@include conditions on the last field of `op_path` and on the
/// inline fragments after it, as condition-only fragments, which a key hop
/// must carry into the entity fetch's op path. Type conditions are dropped:
/// the entity root and the key inputs already narrow the type, and the type
/// may not exist in the entity's subgraph (an @interfaceObject's
/// implementations).
///
/// FIXME: conditions above the last field are not propagated across key
/// hops. See `non_trailing_skip_is_dropped`.
pub(super) fn trailing_condition_fragments(
    op_path: &SharedPath<Arc<OpPathElement>>,
) -> Vec<Arc<OpPathElement>> {
    let mut trailing = Vec::new();
    for element in op_path.iter() {
        match element.as_ref() {
            OpPathElement::Field(field) => {
                trailing.clear();
                let conditions = condition_directives(&field.directives);
                if conditions.is_empty() {
                    continue;
                }
                let Ok(Ok(field_type)) = field
                    .output_base_type()
                    .map(CompositeTypeDefinitionPosition::try_from)
                else {
                    continue;
                };
                trailing.push(Arc::new(OpPathElement::InlineFragment(InlineFragment {
                    schema: field.schema.clone(),
                    parent_type_position: field_type,
                    type_condition_position: None,
                    directives: conditions,
                    selection_id: SelectionId::new(),
                })));
            }
            OpPathElement::InlineFragment(frag) => {
                let conditions = condition_directives(&frag.directives);
                if conditions.is_empty() {
                    continue;
                }
                trailing.push(Arc::new(OpPathElement::InlineFragment(
                    frag.with_updated_directives(conditions)
                        .with_updated_type_condition(None),
                )));
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
    input_path_keeping(op_path, &DirectiveList::new())
}

/// `op_path` for appending the key inputs of an entity fetch gated on the
/// @skip/@include of `trailing_condition_fragments(op_path)`: like
/// `unconditioned_input_path`, but the conditions the fetch is gated on are
/// kept. The fetch only runs when they hold, so its inputs are there whenever
/// it reads them, and a fetch whose selections all sit under one such
/// condition (a re-fetch lifted under it) can be gated on it as well.
pub(super) fn gated_input_path(
    op_path: &SharedPath<Arc<OpPathElement>>,
) -> SharedPath<Arc<OpPathElement>> {
    let gate: DirectiveList = trailing_condition_fragments(op_path)
        .iter()
        .flat_map(|element| match element.as_ref() {
            OpPathElement::InlineFragment(frag) => frag.directives.iter().cloned().collect(),
            OpPathElement::Field(_) => Vec::new(),
        })
        .collect();
    input_path_keeping(op_path, &gate)
}

/// `op_path` with the @skip/@include not in `kept` stripped from its
/// inline-fragment elements.
fn input_path_keeping(
    op_path: &SharedPath<Arc<OpPathElement>>,
    kept: &DirectiveList,
) -> SharedPath<Arc<OpPathElement>> {
    let mut elements = Vec::with_capacity(op_path.len());
    for element in op_path.iter() {
        match element.as_ref() {
            OpPathElement::Field(_) => elements.push(element.clone()),
            OpPathElement::InlineFragment(frag) => {
                let stripped: DirectiveList = frag
                    .directives
                    .iter()
                    .filter(|d| {
                        (d.name != "skip" && d.name != "include") || kept.iter().any(|k| k == *d)
                    })
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

// ---------------------------------------------------------------------------
// Methods on FieldRoutingSearchSpace
// ---------------------------------------------------------------------------

impl FieldRoutingSearchSpace {
    /// Alias the condition fields whose response key, at the position the
    /// conditions are selected, could hold a different field.
    fn stage_condition_aliases(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
        requires_conditions: &Arc<SelectionSet>,
    ) -> Result<AliasedConditions, FederationError> {
        let merge_at = self.pending_merge_at(state, pending);
        alias_condition_fields(
            requires_conditions,
            &mut state.condition_alias_ids,
            |field| self.response_keys.condition_needs_alias(&merge_at, field),
        )
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

        if choice.is_key_hop() {
            let source = self.node_source(pending.query_graph_node)?;
            let (conditions, alias_rewrites) =
                self.stage_condition_aliases(state, pending, requires_conditions)?;
            let conditions = match key_hop_edge {
                Some(edge) => widen_aliases_with_key_inputs(
                    &conditions,
                    &alias_rewrites,
                    &state.graph,
                    edge,
                    source.type_pos.type_name(),
                )?,
                None => conditions,
            };
            let input_conflict = key_hop_edge.is_some_and(|e| {
                Self::has_conflicting_requires_inputs(&state.graph, e, &conditions, &alias_rewrites)
            });
            let input = InputContribution::Requires {
                source_type_name: source.type_pos.type_name().clone(),
                conditions: conditions.clone(),
                condition_alias_rewrites: alias_rewrites,
            };
            let dest = if input_conflict {
                // The original target.fetch_node is abandoned: it retains its
                // key-hop edge and inputs but receives no query selections, so
                // plan_builder's empty-selection pruning drops it.
                let merge_at = state.graph.merge_at(target.fetch_node).to_vec();
                let split_group = state.graph.add_entity_group_with_defer(
                    choice.target_subgraph(),
                    merge_at,
                    pending.defer_ref.clone(),
                );
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

            // Split the staged conditions: aliasing keeps field names, so a
            // connector anchor maps the same fields with or without aliases.
            let (in_place, routed) = self.split_in_place(
                state,
                pending,
                &conditions,
                choice.requires_resolvable_in_place(),
            );
            if let Some(in_place) = &in_place {
                state.graph.append_selection(
                    pending.fetch_node,
                    &unconditioned_input_path(&pending.op_path),
                    Some(in_place),
                );
                state
                    .graph
                    .add_ordering_dependency(pending.fetch_node, dest)?;
            }
            if let Some(routed) = &routed {
                self.push_condition_pendings(state, pending, routed, dest)?;
            }
            return Ok(CommitTarget {
                fetch_node: dest,
                ..target
            });
        }

        if !choice.requires_resolvable_in_place() {
            return Err(FederationError::internal(
                "non-hop routing choice with @requires conditions not resolvable in place",
            ));
        }

        // In place: the fields sit alongside the field. No entity
        // representation reads them, so aliases need no rename back.
        let (conditions, _) = self.stage_condition_aliases(state, pending, requires_conditions)?;
        state.graph.append_selection(
            target.fetch_node,
            &unconditioned_input_path(&pending.op_path),
            Some(&conditions),
        );
        Ok(target)
    }

    /// Whether adding these inputs to `edge` would put two different fields
    /// under one key of the entity representation. An alias is renamed back
    /// by removing and reinserting the key, so it replaces whatever another
    /// input put there.
    fn has_conflicting_requires_inputs(
        graph: &super::super::fetch_graph::FetchGraph,
        edge: EdgeIndex,
        new_conditions: &Arc<SelectionSet>,
        new_rewrites: &[(Name, Name)],
    ) -> bool {
        if graph.has_conflicting_condition_rewrites(edge, new_rewrites) {
            return true;
        }
        graph.edge_weight_raw(edge).inputs.iter().any(|input| {
            let rewrites = input.condition_alias_rewrites();
            if !rewrites.is_empty() {
                return Self::aliased_input_collides_with_plain(rewrites, new_conditions);
            }
            Self::plain_input_has_argument_conflict(input.conditions(), new_conditions)
                // Splitting cannot separate a condition from the key it
                // enters with, so key inputs are handled by widening the
                // alias instead (`widen_aliases_with_key_inputs`).
                || (input.rewrite_info().is_none()
                    && Self::aliased_input_collides_with_plain(new_rewrites, input.conditions()))
        })
    }

    /// Two plain inputs share a response name but differ in arguments.
    fn plain_input_has_argument_conflict(
        existing_conditions: &SelectionSet,
        new_conditions: &Arc<SelectionSet>,
    ) -> bool {
        for existing_sel in existing_conditions.selections.values() {
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
        false
    }

    /// A plain condition's response name matches a rewrite's original name,
    /// so the rename-back would overwrite the plain data at runtime.
    fn aliased_input_collides_with_plain(
        rewrites: &[(Name, Name)],
        plain_conditions: &SelectionSet,
    ) -> bool {
        rewrites.iter().any(|(_alias, original)| {
            plain_conditions.selections.values().any(
                |sel| matches!(sel, Selection::Field(field) if field.field.response_name() == original),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;

    use super::*;
    use crate::operation::Field;
    use crate::operation::InlineFragment;
    use crate::operation::SelectionId;
    use crate::schema::ValidFederationSchema;
    use crate::schema::position::CompositeTypeDefinitionPosition;
    use crate::schema::position::ObjectTypeDefinitionPosition;

    fn fragment_with_directive(directive: &str) -> Arc<OpPathElement> {
        let schema = apollo_compiler::schema::Schema::parse_and_validate(
            "type Query { x: Int }",
            "schema.graphql",
        )
        .expect("valid schema");
        let schema = ValidFederationSchema::new(schema).expect("valid federation schema");
        let parent: CompositeTypeDefinitionPosition =
            CompositeTypeDefinitionPosition::Object(ObjectTypeDefinitionPosition {
                type_name: name!("Query"),
            });
        let directives: DirectiveList = std::iter::once(apollo_compiler::ast::Directive::new(
            Name::new(directive).expect("valid name"),
        ))
        .collect();
        Arc::new(OpPathElement::InlineFragment(InlineFragment {
            schema,
            parent_type_position: parent.clone(),
            type_condition_position: Some(parent),
            directives,
            selection_id: SelectionId::new(),
        }))
    }

    fn field_element() -> Arc<OpPathElement> {
        let schema = apollo_compiler::schema::Schema::parse_and_validate(
            "type Query { x: Int }",
            "schema.graphql",
        )
        .expect("valid schema");
        let schema = ValidFederationSchema::new(schema).expect("valid federation schema");
        Arc::new(OpPathElement::Field(Field {
            schema,
            field_position: crate::schema::position::FieldDefinitionPosition::Object(
                ObjectTypeDefinitionPosition {
                    type_name: name!("Query"),
                }
                .field(name!("x")),
            ),
            alias: None,
            arguments: Default::default(),
            directives: Default::default(),
            sibling_typename: None,
        }))
    }

    /// Only @skip/@include are Boolean conditions a key hop must replay
    /// into the entity fetch. A fragment carrying an unrelated directive
    /// (e.g. @defer) is not a condition, matching the filter used by
    /// `unconditioned_input_path`.
    #[test]
    fn trailing_condition_fragments_only_match_skip_include() {
        let skip = SharedPath::new().pushed(fragment_with_directive("skip"));
        assert_eq!(trailing_condition_fragments(&skip).len(), 1);

        let other = SharedPath::new().pushed(fragment_with_directive("defer"));
        assert_eq!(
            trailing_condition_fragments(&other).len(),
            0,
            "a non-condition directive must not be treated as @skip/@include",
        );
    }

    /// A @skip/@include before (not after) the last field is dropped.
    /// The JS planner propagates all path conditions via OpGraphPathContext;
    /// this helper only captures trailing ones, so a non-trailing condition
    /// can be bypassed when a separate unconditional path satisfies the
    /// same data dependency.
    #[test]
    fn non_trailing_skip_is_dropped() {
        let path = SharedPath::new()
            .pushed(fragment_with_directive("skip"))
            .pushed(field_element());
        assert_eq!(
            trailing_condition_fragments(&path).len(),
            0,
            "known gap: non-trailing @skip is not propagated across key hops",
        );
    }

    /// Only top-level fields get aliases; a fragment keeps the names of the
    /// fields inside it. A field with the same identity reuses its alias.
    #[test]
    fn alias_condition_fields_aliases_fields_and_keeps_fragments() {
        let sdl =
            "type Query { i: I } interface I { a: Int } type A implements I { a: Int b: Int }";
        let schema = apollo_compiler::schema::Schema::parse_and_validate(sdl, "schema.graphql")
            .expect("valid schema");
        let schema = ValidFederationSchema::new(schema).expect("valid federation schema");
        let interface = schema.get_type(&name!("I")).expect("I exists");
        let conditions = SelectionSet::parse(
            schema,
            interface.try_into().expect("I is composite"),
            "a ... on A { b }",
        )
        .expect("conditions parse");
        let conditions = Arc::new(conditions);
        let mut alias_ids = Vec::new();
        for _ in 0..2 {
            let (aliased, rewrites) =
                alias_condition_fields(&conditions, &mut alias_ids, |_| true).expect("aliases");
            assert_eq!(aliased.to_string(), "{ __require_0_a: a ... on A { b } }");
            assert_eq!(rewrites, vec![(name!("__require_0_a"), name!("a"))]);
        }
    }
}
