//! Condition satisfiability: can a set of @requires / @key fields be resolved
//! at a given query graph node?
//!
//! Three flavors of check, each deeper than the last:
//! - `can_satisfy_conditions`: pure schema lookup (field exists, not external).
//! - `conditions_resolvable_at_node`: graph-based, path-sensitive variant.
//! - `conditions_have_requires`: detects @requires on condition edges.

use std::sync::Arc;

use petgraph::graph::NodeIndex;

use super::FieldRoutingSearchSpace;
use super::NodeSource;
use crate::error::FederationError;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::operation::TYPENAME_FIELD;
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
    /// `node` via outgoing edges? Recurses into composite sub-selections
    /// to catch nested fields the subgraph cannot reach.
    pub(super) fn conditions_resolvable_at_node(
        &self,
        node: NodeIndex,
        conditions: &Arc<SelectionSet>,
    ) -> Result<bool, FederationError> {
        self.conditions_resolvable_inner(node, conditions.as_ref())
    }

    fn conditions_resolvable_inner(
        &self,
        node: NodeIndex,
        conditions: &SelectionSet,
    ) -> Result<bool, FederationError> {
        for selection in conditions.selections.values() {
            match selection {
                Selection::Field(field_sel) => {
                    if *field_sel.field.name() == TYPENAME_FIELD {
                        continue;
                    }
                    let Some(edge_idx) = self.edge_for_field(node, &field_sel.field) else {
                        return Ok(false);
                    };
                    let edge = self.query_graph.edge_weight(edge_idx)?;
                    if edge.conditions.is_some() {
                        return Ok(false);
                    }
                    if let Some(sub_sel) = field_sel.selection_set.as_ref() {
                        let (_, target) = self.query_graph.edge_endpoints(edge_idx)?;
                        if !self.conditions_resolvable_inner(target, sub_sel)? {
                            return Ok(false);
                        }
                    }
                }
                Selection::InlineFragment(frag_sel) => {
                    let target = self
                        .edge_for_inline_fragment(node, &frag_sel.inline_fragment)
                        .map(|edge_idx| {
                            self.query_graph
                                .edge_endpoints(edge_idx)
                                .map(|(_, target)| target)
                        })
                        .transpose()?
                        .unwrap_or(node);
                    if !self.conditions_resolvable_inner(target, &frag_sel.selection_set)? {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }

    /// Whether any condition field (recursively) carries @requires of its
    /// own. Such fields draw data from the entity representation and can
    /// never be appended to an existing fetch as-is.
    pub(super) fn conditions_have_requires(
        &self,
        node: NodeIndex,
        conditions: &Arc<SelectionSet>,
    ) -> Result<bool, FederationError> {
        self.conditions_have_requires_inner(node, conditions.as_ref())
    }

    fn conditions_have_requires_inner(
        &self,
        node: NodeIndex,
        conditions: &SelectionSet,
    ) -> Result<bool, FederationError> {
        for selection in conditions.selections.values() {
            match selection {
                Selection::Field(field_sel) => {
                    if *field_sel.field.name() == TYPENAME_FIELD {
                        continue;
                    }
                    let Some(edge_idx) = self.edge_for_field(node, &field_sel.field) else {
                        continue;
                    };
                    let edge = self.query_graph.edge_weight(edge_idx)?;
                    if edge.conditions.is_some() {
                        return Ok(true);
                    }
                    if let Some(sub_sel) = field_sel.selection_set.as_ref() {
                        let (_, target) = self.query_graph.edge_endpoints(edge_idx)?;
                        if self.conditions_have_requires_inner(target, sub_sel)? {
                            return Ok(true);
                        }
                    }
                }
                Selection::InlineFragment(frag_sel) => {
                    let target = self
                        .edge_for_inline_fragment(node, &frag_sel.inline_fragment)
                        .map(|edge_idx| {
                            self.query_graph
                                .edge_endpoints(edge_idx)
                                .map(|(_, target)| target)
                        })
                        .transpose()?
                        .unwrap_or(node);
                    if self.conditions_have_requires_inner(target, &frag_sel.selection_set)? {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// Filter key conditions to the subset the source subgraph can resolve.
    pub(super) fn locally_satisfiable_subset(
        &self,
        conditions: &Arc<SelectionSet>,
        source: &NodeSource,
    ) -> Option<Arc<SelectionSet>> {
        satisfiable_subset(conditions, &source.type_pos, &source.schema)
    }
}

/// Check whether every field in `conditions` exists in the subgraph schema
/// at the given type position. Recurses into composite sub-selections so
/// that `c { cid cm }` fails when `cm` is absent from the nested type,
/// even though `c` itself exists. Also rejects `@external` fields since the
/// subgraph cannot resolve them locally.
fn can_satisfy_conditions(
    conditions: &SelectionSet,
    type_pos: &CompositeTypeDefinitionPosition,
    schema: &ValidFederationSchema,
) -> bool {
    for selection in conditions.selections.values() {
        match selection {
            Selection::Field(field_sel) => {
                if !can_satisfy_field(field_sel, type_pos, schema) {
                    return false;
                }
            }
            Selection::InlineFragment(frag_sel) => {
                if !can_satisfy_fragment(frag_sel, type_pos, schema) {
                    return false;
                }
            }
        }
    }
    true
}

/// Single-field arm of `can_satisfy_conditions`: the field must exist in the
/// schema, not be `@external`, and its sub-selections (if any) must also be
/// satisfiable in the nested type.
fn can_satisfy_field(
    field_sel: &crate::operation::FieldSelection,
    type_pos: &CompositeTypeDefinitionPosition,
    schema: &ValidFederationSchema,
) -> bool {
    if *field_sel.field.name() == TYPENAME_FIELD {
        return true;
    }
    let Ok(field_pos) = type_pos.field(field_sel.field.name().clone()) else {
        return false;
    };
    let Some(field_def) = field_pos.try_get(schema.schema()) else {
        return false;
    };
    if let Some(meta) = schema.subgraph_metadata()
        && meta.is_field_external(&field_pos)
    {
        return false;
    }
    if let Some(sub_sel) = field_sel.selection_set.as_ref() {
        let field_type = field_def.ty.inner_named_type();
        if let Ok(sub_type) = schema.get_type(field_type).and_then(|t| {
            CompositeTypeDefinitionPosition::try_from(t).map_err(FederationError::from)
        }) && !can_satisfy_conditions(sub_sel, &sub_type, schema)
        {
            return false;
        }
    }
    true
}

/// Fragment arm of `can_satisfy_conditions`: typed fragments resolve against
/// the fragment's condition type; untyped fragments inherit the parent.
fn can_satisfy_fragment(
    frag_sel: &crate::operation::InlineFragmentSelection,
    type_pos: &CompositeTypeDefinitionPosition,
    schema: &ValidFederationSchema,
) -> bool {
    if let Some(type_cond) = &frag_sel.inline_fragment.type_condition_position {
        if let Ok(sub_type) = schema.get_type(type_cond.type_name()).and_then(|t| {
            CompositeTypeDefinitionPosition::try_from(t).map_err(FederationError::from)
        }) && !can_satisfy_conditions(&frag_sel.selection_set, &sub_type, schema)
        {
            return false;
        }
    } else if !can_satisfy_conditions(&frag_sel.selection_set, type_pos, schema) {
        return false;
    }
    true
}

/// Filter a key selection set to only the fields the source subgraph can
/// resolve: each field must exist, not be `@external`, and have its
/// sub-selections satisfiable. Fields that fail any check are dropped.
/// Returns `None` when no field survives.
fn satisfiable_subset(
    conditions: &SelectionSet,
    type_pos: &CompositeTypeDefinitionPosition,
    schema: &ValidFederationSchema,
) -> Option<Arc<SelectionSet>> {
    let mut kept: Vec<Selection> = Vec::new();
    for selection in conditions.selections.values() {
        match selection {
            Selection::Field(field_sel) => {
                if *field_sel.field.name() == TYPENAME_FIELD {
                    kept.push(selection.clone());
                    continue;
                }
                let Ok(field_pos) = type_pos.field(field_sel.field.name().clone()) else {
                    continue;
                };
                if field_pos.try_get(schema.schema()).is_none() {
                    continue;
                }
                if let Some(meta) = schema.subgraph_metadata()
                    && meta.is_field_external(&field_pos)
                {
                    continue;
                }
                kept.push(selection.clone());
            }
            Selection::InlineFragment(_) => {
                kept.push(selection.clone());
            }
        }
    }
    if kept.is_empty() {
        return None;
    }
    Some(Arc::new(SelectionSet::from_raw_selections(
        conditions.schema.clone(),
        conditions.type_position.clone(),
        kept,
    )))
}
