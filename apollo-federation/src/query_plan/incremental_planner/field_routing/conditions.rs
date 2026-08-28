//! Condition satisfiability: can a set of @requires / @key fields be resolved
//! at a given query graph node?

use std::sync::Arc;

use petgraph::graph::NodeIndex;

use super::FieldRoutingSearchSpace;
use crate::error::FederationError;
use crate::operation::SelectionSet;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;

impl FieldRoutingSearchSpace {
    /// Schema-based check: can every field in `conditions` be found in the
    /// subgraph's schema at `type_pos`?
    pub(super) fn can_satisfy(
        &self,
        conditions: &Arc<SelectionSet>,
        type_pos: &CompositeTypeDefinitionPosition,
        _subgraph: &Arc<str>,
        schema: &ValidFederationSchema,
    ) -> bool {
        can_satisfy_conditions(conditions, type_pos, schema)
    }

    /// Graph-based check: can every field in `conditions` be resolved at
    /// `node` via outgoing edges?
    pub(super) fn conditions_resolvable_at_node(
        &self,
        node: NodeIndex,
        conditions: &Arc<SelectionSet>,
    ) -> Result<bool, FederationError> {
        for selection in conditions.selections.values() {
            match selection {
                crate::operation::Selection::Field(field_sel) => {
                    if self.edge_for_field(node, &field_sel.field).is_none() {
                        return Ok(false);
                    }
                }
                crate::operation::Selection::InlineFragment(frag_sel) => {
                    if self
                        .edge_for_inline_fragment(node, &frag_sel.inline_fragment)
                        .is_none()
                    {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }

    /// Do any fields in `conditions` carry @requires at `node`? If so,
    /// the conditions cannot be resolved in-place and need their own
    /// entity fetch.
    pub(super) fn conditions_have_requires(
        &self,
        node: NodeIndex,
        conditions: &Arc<SelectionSet>,
    ) -> Result<bool, FederationError> {
        for selection in conditions.selections.values() {
            if let crate::operation::Selection::Field(field_sel) = selection
                && let Some(edge_idx) = self.edge_for_field(node, &field_sel.field)
            {
                let edge = self.query_graph.edge_weight(edge_idx)?;
                if edge.conditions.is_some() {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

/// Check whether every field in `conditions` exists in the subgraph schema
/// at the given type position.
fn can_satisfy_conditions(
    conditions: &SelectionSet,
    type_pos: &CompositeTypeDefinitionPosition,
    schema: &ValidFederationSchema,
) -> bool {
    for selection in conditions.selections.values() {
        match selection {
            crate::operation::Selection::Field(field_sel) => {
                let field_name = field_sel.field.name();
                if field_name.as_str() == "__typename" {
                    continue;
                }
                let field_exists = type_pos
                    .field(field_name.clone())
                    .is_ok_and(|field_pos| field_pos.get(schema.schema()).is_ok());
                if !field_exists {
                    return false;
                }
            }
            crate::operation::Selection::InlineFragment(frag_sel) => {
                if let Some(type_cond) = &frag_sel.inline_fragment.type_condition_position {
                    let inner_type_name = type_cond.type_name();
                    let inner_pos = schema
                        .get_type(inner_type_name)
                        .ok()
                        .and_then(|ty| CompositeTypeDefinitionPosition::try_from(ty).ok());
                    match inner_pos {
                        Some(pos) => {
                            if !can_satisfy_conditions(&frag_sel.selection_set, &pos, schema) {
                                return false;
                            }
                        }
                        None => return false,
                    }
                }
            }
        }
    }
    true
}
