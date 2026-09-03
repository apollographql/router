use std::sync::Arc;

use apollo_compiler::name;
use petgraph::graph::NodeIndex;

use crate::error::FederationError;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;

const TYPENAME_FIELD: apollo_compiler::Name = name!("__typename");

use super::FieldRoutingSearchSpace;

impl FieldRoutingSearchSpace {
    /// Schema-based check: the conditions are locally satisfiable if every
    /// field in the selection set exists in the subgraph schema and is not
    /// @external.
    pub(super) fn can_satisfy(
        &self,
        conditions: &SelectionSet,
        source_type: &CompositeTypeDefinitionPosition,
        source_schema: &ValidFederationSchema,
    ) -> bool {
        can_satisfy_conditions(conditions, source_type, source_schema)
    }

    /// Cached wrapper around `can_satisfy`: keyed by (Arc pointer of
    /// conditions, type name, subgraph name) so repeated checks for the
    /// same condition set at the same position short-circuit.
    pub(super) fn cached_can_satisfy(
        &self,
        conditions: &Arc<SelectionSet>,
        type_pos: &CompositeTypeDefinitionPosition,
        subgraph: &Arc<str>,
        schema: &ValidFederationSchema,
    ) -> bool {
        let key = (
            super::ConditionsKey::new(conditions),
            type_pos.type_name().clone(),
            subgraph.clone(),
        );
        if let Some(&cached) = self.caches.can_satisfy.borrow().get(&key) {
            return cached;
        }
        let result = self.can_satisfy(conditions, type_pos, schema);
        self.caches.can_satisfy.borrow_mut().insert(key, result);
        result
    }

    /// Graph-based check: can every field in `conditions` be resolved at
    /// `node` via outgoing edges? Recurses into composite sub-selections
    /// to catch nested fields the subgraph cannot reach. Path-sensitive
    /// where the schema check is not: an @external field still has a real
    /// edge on the provides-copy node created by an ancestor's @provides,
    /// so conditions like a same-subgraph @requires can resolve in place
    /// there.
    pub(super) fn conditions_resolvable_at_node(
        &self,
        node: NodeIndex,
        conditions: &SelectionSet,
    ) -> Result<bool, FederationError> {
        self.walk_conditions_graph(node, conditions, true)
    }

    /// Walk condition fields via graph edges. When `fail_on_unreachable` is
    /// true, returns false if any field lacks an edge (resolvability check).
    /// When false, skips missing fields and returns true if any edge carries
    /// conditions (requires detection).
    fn walk_conditions_graph(
        &self,
        node: NodeIndex,
        conditions: &SelectionSet,
        fail_on_unreachable: bool,
    ) -> Result<bool, FederationError> {
        for selection in conditions.selections.values() {
            match selection {
                Selection::Field(field_sel) => {
                    if *field_sel.field.name() == TYPENAME_FIELD {
                        continue;
                    }
                    let Some(edge_idx) = self
                        .cached_query_graph
                        .edge_for_field(node, &field_sel.field)
                    else {
                        if fail_on_unreachable {
                            return Ok(false);
                        }
                        continue;
                    };
                    let edge = self.qg().edge_weight(edge_idx)?;
                    if edge.conditions.is_some() {
                        return Ok(!fail_on_unreachable);
                    }
                    if let Some(sub_sel) = field_sel.selection_set.as_ref() {
                        let (_, target) = self.qg().edge_endpoints(edge_idx)?;
                        let sub_result =
                            self.walk_conditions_graph(target, sub_sel, fail_on_unreachable)?;
                        if sub_result != fail_on_unreachable {
                            return Ok(sub_result);
                        }
                    }
                }
                Selection::InlineFragment(frag_sel) => {
                    let target = self
                        .cached_query_graph
                        .edge_for_inline_fragment(node, &frag_sel.inline_fragment)
                        .map(|edge_idx| {
                            self.qg().edge_endpoints(edge_idx).map(|(_, target)| target)
                        })
                        .transpose()?
                        .unwrap_or(node);
                    let sub_result = self.walk_conditions_graph(
                        target,
                        &frag_sel.selection_set,
                        fail_on_unreachable,
                    )?;
                    if sub_result != fail_on_unreachable {
                        return Ok(sub_result);
                    }
                }
            }
        }
        Ok(fail_on_unreachable)
    }

    /// Whether any condition field (recursively) carries @requires of its
    /// own. Such fields draw data from the entity representation and can
    /// never be appended to an existing fetch as-is.
    pub(super) fn conditions_have_requires(
        &self,
        node: NodeIndex,
        conditions: &Arc<SelectionSet>,
    ) -> Result<bool, FederationError> {
        self.walk_conditions_graph(node, conditions.as_ref(), false)
    }

    /// Filter a key selection set to only the fields the source subgraph can
    /// resolve locally: each field must exist and not be @external. Returns
    /// `None` when no field survives.
    pub(super) fn locally_satisfiable_subset(
        &self,
        conditions: &SelectionSet,
        source_type: &CompositeTypeDefinitionPosition,
        source_schema: &ValidFederationSchema,
    ) -> Option<Arc<SelectionSet>> {
        satisfiable_subset(conditions, source_type, source_schema)
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
