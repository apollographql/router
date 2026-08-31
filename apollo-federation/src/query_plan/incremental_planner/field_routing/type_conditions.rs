//! Handling for selections with no direct routing option: pass-through
//! and vacuous fragments, and type explosion of abstract types into
//! concrete-type fragments. Enumeration offers these as forced fallback
//! choices (`RoutingTarget::RestructureFragment` / `TypeExplosion`);
//! `commit_choice` dispatches into the `try_*` functions here.

use std::sync::Arc;

use tracing::debug;
use tracing::trace;

use super::FieldRoutingSearchSpace;
use super::state::PendingSelection;
use super::state::PlanState;
use crate::error::FederationError;
use crate::operation::DirectiveList;
use crate::operation::InlineFragment;
use crate::operation::InlineFragmentSelection;
use crate::operation::Selection;
use crate::operation::SelectionId;
use crate::operation::SelectionMap;
use crate::operation::SelectionSet;
use crate::query_graph::QueryGraphNodeType;
use crate::query_graph::graph_path::operation::OpPathElement;
use crate::query_plan::conditions::Conditions;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;

/// For each concrete type, push a pending `InlineFragment` conditioned on
/// that type wrapping the result of `make_selection_set`. Shared by
/// `try_explode_abstract_type` and `try_explode_interface_field`.
fn push_concrete_type_fragments<'a>(
    state: &mut PlanState,
    pending: &PendingSelection,
    concrete_types: impl DoubleEndedIterator<Item = &'a ObjectTypeDefinitionPosition>,
    schema: &ValidFederationSchema,
    parent_type: &CompositeTypeDefinitionPosition,
    directives: DirectiveList,
    make_selection_set: impl Fn(&InlineFragment) -> SelectionSet,
) {
    for concrete_type in concrete_types.rev() {
        let concrete_composite: CompositeTypeDefinitionPosition = concrete_type.clone().into();
        let frag = InlineFragment {
            schema: schema.clone(),
            parent_type_position: parent_type.clone(),
            type_condition_position: Some(concrete_composite),
            directives: directives.clone(),
            selection_id: SelectionId::new(),
        };
        let selection_set = make_selection_set(&frag);
        let wrapped = Selection::InlineFragment(Arc::new(InlineFragmentSelection {
            inline_fragment: frag,
            selection_set,
        }));
        state.push_pending(pending.fork(wrapped));
    }
}

impl FieldRoutingSearchSpace {
    /// For an inline fragment with no type condition (which has no query
    /// graph edge), push its sub-selections at the same query graph node.
    pub(super) fn try_pass_through_fragment(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
    ) -> Result<bool, FederationError> {
        if let Selection::InlineFragment(frag_sel) = &pending.selection
            && frag_sel.inline_fragment.type_condition_position.is_none()
        {
            // Preserve remaining directives (@skip/@include): the fragment
            // has no edge, but its conditions gate every child, so a
            // condition-carrying element must ride the children's op path.
            // Constant conditions resolve statically: always-false drops
            // the children, always-true is a plain pass-through.
            let child_op_path =
                match Conditions::from_directives(&frag_sel.inline_fragment.directives)? {
                    Conditions::Boolean(false) => return Ok(true),
                    Conditions::Boolean(true) => pending.op_path.clone(),
                    Conditions::Variables(_) => {
                        pending
                            .op_path
                            .pushed(Arc::new(OpPathElement::InlineFragment(
                                frag_sel.inline_fragment.clone(),
                            )))
                    }
                };
            for sub_sel in frag_sel.selection_set.selections.values().rev().cloned() {
                state.push_pending(pending.fork(sub_sel).with_op_path(child_op_path.clone()));
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Vacuous type condition: the current node's runtime types are a subset
    /// of the condition's (e.g. `...on Node` when every union member
    /// implements Node), so treat the fragment as pass-through, preserving
    /// non-routing directives by pushing them onto the op_path.
    pub(super) fn try_vacuous_type_condition(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
    ) -> Result<bool, FederationError> {
        if let Selection::InlineFragment(frag_sel) = &pending.selection
            && let Some(type_cond) = &frag_sel.inline_fragment.type_condition_position
        {
            let current_node = self.query_graph.node_weight(pending.query_graph_node)?;

            let is_vacuous =
                if matches!(current_node.type_, QueryGraphNodeType::FederatedRootType(_)) {
                    true
                } else {
                    let current_type: CompositeTypeDefinitionPosition =
                        current_node.type_.clone().try_into()?;
                    let current_schema = self.query_graph.schema_by_source(&current_node.source)?;
                    let current_runtime_types =
                        current_schema.possible_runtime_types(current_type.clone())?;
                    let cond_runtime_types = self
                        .supergraph_schema
                        .possible_runtime_types(type_cond.clone())?;
                    current_runtime_types.is_subset(&cond_runtime_types)
                };

            if is_vacuous {
                trace!(
                    type_condition = %type_cond,
                    "vacuous type condition -- treating as pass-through",
                );
                let directives = &frag_sel.inline_fragment.directives;
                let child_op_path = if !directives.is_empty() {
                    let stripped = frag_sel.inline_fragment.with_updated_type_condition(None);
                    pending
                        .op_path
                        .pushed(Arc::new(OpPathElement::InlineFragment(stripped)))
                } else {
                    pending.op_path.clone()
                };
                for sub_sel in frag_sel.selection_set.selections.values().rev().cloned() {
                    state.push_pending(pending.fork(sub_sel).with_op_path(child_op_path.clone()));
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Type explosion: the condition is an abstract type whose runtime types
    /// partially overlap the current node's -- decompose into
    /// concrete-type fragments.
    pub(super) fn try_explode_abstract_type(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
    ) -> Result<bool, FederationError> {
        if let Selection::InlineFragment(frag_sel) = &pending.selection
            && let Some(type_cond) = &frag_sel.inline_fragment.type_condition_position
        {
            let current_node = self.query_graph.node_weight(pending.query_graph_node)?;
            let current_type: CompositeTypeDefinitionPosition =
                current_node.type_.clone().try_into()?;
            let current_schema = self.query_graph.schema_by_source(&current_node.source)?;

            let current_runtime_types =
                current_schema.possible_runtime_types(current_type.clone())?;
            let cond_runtime_types = self
                .supergraph_schema
                .possible_runtime_types(type_cond.clone())?;

            let intersection: Vec<_> = current_runtime_types
                .intersection(&cond_runtime_types)
                .cloned()
                .collect();

            if intersection.is_empty() {
                trace!(
                    type_condition = %type_cond,
                    "type condition has empty local runtime intersection -- dropping fragment",
                );
                return Ok(true);
            }

            // Progress guard: rewriting `... on T` into `... on T` would
            // push the exact selection we just popped, looping forever in
            // fast_forward. Fall through to drop handling instead.
            if intersection.len() == 1 && intersection[0].type_name == *type_cond.type_name() {
                return Ok(false);
            }

            trace!(
                type_condition = %type_cond,
                concrete_types = ?intersection.iter().map(|t| t.type_name.as_str()).collect::<Vec<_>>(),
                "type-exploding abstract type condition into concrete types",
            );
            let sel_set = frag_sel.selection_set.clone();
            push_concrete_type_fragments(
                state,
                pending,
                intersection.iter(),
                &frag_sel.inline_fragment.schema,
                &frag_sel.inline_fragment.parent_type_position,
                frag_sel.inline_fragment.directives.clone(),
                |_| sel_set.clone(),
            );
            return Ok(true);
        }
        Ok(false)
    }

    /// Field on an abstract type with no direct FieldCollection edge
    /// (e.g. not all local implementors provide the interface field):
    /// decompose into per-concrete-type inline fragments so each concrete
    /// type routes the field independently, possibly to different subgraphs.
    pub(super) fn try_explode_interface_field(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
    ) -> Result<bool, FederationError> {
        if let Selection::Field(field_sel) = &pending.selection {
            let current_node = self.query_graph.node_weight(pending.query_graph_node)?;
            if let Ok(current_type) =
                CompositeTypeDefinitionPosition::try_from(current_node.type_.clone())
                && current_type.is_abstract_type()
            {
                let current_schema = self.query_graph.schema_by_source(&current_node.source)?;
                if let Ok(runtime_types) =
                    current_schema.possible_runtime_types(current_type.clone())
                {
                    let exploded = !runtime_types.is_empty();
                    let inner_sel = Selection::Field(field_sel.clone());
                    let schema = field_sel.field.schema();
                    push_concrete_type_fragments(
                        state,
                        pending,
                        runtime_types.iter(),
                        schema,
                        &current_type,
                        Default::default(),
                        |frag| {
                            let mut inner_map = SelectionMap::new();
                            inner_map.insert(inner_sel.clone());
                            SelectionSet {
                                schema: schema.clone(),
                                type_position: frag.casted_type(),
                                selections: Arc::new(inner_map),
                            }
                        },
                    );
                    if exploded {
                        trace!(
                            field = %field_sel.field.field_position,
                            "type-exploded interface field into concrete-type fragments",
                        );
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// Log and count a truly unresolvable selection so the cost function
    /// penalizes this plan and BULB backtracks. Best-effort selections are
    /// dropped without the count (their loss must not fail the plan).
    pub(super) fn drop_unresolvable(&self, state: &mut PlanState, pending: &PendingSelection) {
        match &pending.selection {
            Selection::Field(f) => {
                debug!(
                    field = %f.field.field_position,
                    qg_node = ?pending.query_graph_node,
                    "dropped field: no routing options",
                );
            }
            Selection::InlineFragment(f) => {
                debug!(
                    type_condition = ?f.inline_fragment.type_condition_position,
                    qg_node = ?pending.query_graph_node,
                    "dropped fragment: no routing options",
                );
            }
        }

        if !pending.best_effort {
            state.dropped_fields += 1;
        }
    }
}
