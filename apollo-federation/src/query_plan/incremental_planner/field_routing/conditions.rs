//! Condition satisfiability: can a set of @requires / @key fields be resolved
//! at a given query graph node?

use petgraph::graph::NodeIndex;

use super::FieldRoutingSearchSpace;
use crate::error::FederationError;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::operation::SelectionSet;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;

impl FieldRoutingSearchSpace {
    /// Schema-based check: can every field in `conditions` (recursively) be
    /// resolved by the subgraph's schema at `type_pos`? Fields that are
    /// @external in the subgraph are defined for validity only and do not
    /// satisfy conditions.
    pub(super) fn can_satisfy(
        &self,
        conditions: &SelectionSet,
        type_pos: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> bool {
        can_satisfy_conditions(conditions, type_pos, schema)
    }

    /// Graph-based check: can every field in `conditions` (recursively) be
    /// resolved at `node` via outgoing edges? Fields recurse through their
    /// edge's tail node; condition-less inline fragments are transparent,
    /// matching [`can_satisfy`](Self::can_satisfy).
    pub(super) fn conditions_resolvable_at_node(
        &self,
        node: NodeIndex,
        conditions: &SelectionSet,
    ) -> Result<bool, FederationError> {
        for selection in conditions.selections.values() {
            match selection {
                crate::operation::Selection::Field(field_sel) => {
                    let Some(edge) = self.edge_for_field(node, &field_sel.field) else {
                        return Ok(false);
                    };
                    if let Some(sub) = &field_sel.selection_set {
                        let (_, tail) = self.query_graph.edge_endpoints(edge)?;
                        if !self.conditions_resolvable_at_node(tail, sub)? {
                            return Ok(false);
                        }
                    }
                }
                crate::operation::Selection::InlineFragment(frag_sel) => {
                    let target = if frag_sel.inline_fragment.type_condition_position.is_some() {
                        let Some(edge) =
                            self.edge_for_inline_fragment(node, &frag_sel.inline_fragment)
                        else {
                            return Ok(false);
                        };
                        self.query_graph.edge_endpoints(edge)?.1
                    } else {
                        node
                    };
                    if !self.conditions_resolvable_at_node(target, &frag_sel.selection_set)? {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }

    /// Do any fields in `conditions` (recursively) carry @requires at
    /// `node`? If so, the conditions cannot be resolved in-place and need
    /// their own entity fetch. Fields without an edge here are ignored:
    /// resolvability is the other checks' job.
    pub(super) fn conditions_have_requires(
        &self,
        node: NodeIndex,
        conditions: &SelectionSet,
    ) -> Result<bool, FederationError> {
        for selection in conditions.selections.values() {
            match selection {
                crate::operation::Selection::Field(field_sel) => {
                    if let Some(edge_idx) = self.edge_for_field(node, &field_sel.field) {
                        if self.query_graph.edge_weight(edge_idx)?.conditions.is_some() {
                            return Ok(true);
                        }
                        if let Some(sub) = &field_sel.selection_set {
                            let (_, tail) = self.query_graph.edge_endpoints(edge_idx)?;
                            if self.conditions_have_requires(tail, sub)? {
                                return Ok(true);
                            }
                        }
                    }
                }
                crate::operation::Selection::InlineFragment(frag_sel) => {
                    // A type-conditioned fragment without a downcast edge
                    // (same-type or supertype spread) collects at this node;
                    // checking here over-approximates safely for unrelated
                    // conditions (fields without edges are ignored anyway).
                    let target = if frag_sel.inline_fragment.type_condition_position.is_some() {
                        match self.edge_for_inline_fragment(node, &frag_sel.inline_fragment) {
                            Some(edge) => self.query_graph.edge_endpoints(edge)?.1,
                            None => node,
                        }
                    } else {
                        node
                    };
                    if self.conditions_have_requires(target, &frag_sel.selection_set)? {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }
}

/// Whether the field definition carries a progressive @override label.
fn has_progressive_override(
    definition: &apollo_compiler::schema::FieldDefinition,
    schema: &ValidFederationSchema,
) -> bool {
    let Ok(spec) = get_federation_spec_definition_from_subgraph(schema) else {
        return false;
    };
    let Ok(directive_definition) = spec.override_directive_definition(schema) else {
        return false;
    };
    definition.directives.iter().any(|d| {
        d.name == directive_definition.name
            && spec
                .override_directive_arguments(d)
                .is_ok_and(|args| args.label.is_some())
    })
}

/// Check whether every field in `conditions` (recursively) is resolvable in
/// the subgraph schema at the given type position. A field that is defined
/// but @external does not count: it exists only to keep the subgraph schema
/// valid and is resolved elsewhere.
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
                let Ok(field_pos) = type_pos.field(field_name.clone()) else {
                    return false;
                };
                let Ok(definition) = field_pos.get(schema.schema()) else {
                    return false;
                };
                if schema
                    .subgraph_metadata()
                    .is_some_and(|meta| meta.external_metadata().is_external(&field_pos))
                {
                    return false;
                }
                // A progressive @override label routes this field per
                // request; only the override-aware graph check can decide.
                if has_progressive_override(definition, schema) {
                    return false;
                }
                if let Some(sub) = &field_sel.selection_set {
                    let inner_pos = schema
                        .get_type(definition.ty.inner_named_type())
                        .ok()
                        .and_then(|ty| CompositeTypeDefinitionPosition::try_from(ty).ok());
                    match inner_pos {
                        Some(pos) => {
                            if !can_satisfy_conditions(sub, &pos, schema) {
                                return false;
                            }
                        }
                        None => return false,
                    }
                }
            }
            crate::operation::Selection::InlineFragment(frag_sel) => {
                // Condition-less fragments are transparent, consistent with
                // the graph-based check.
                let inner_pos = match &frag_sel.inline_fragment.type_condition_position {
                    Some(type_cond) => {
                        let resolved = schema
                            .get_type(type_cond.type_name())
                            .ok()
                            .and_then(|ty| CompositeTypeDefinitionPosition::try_from(ty).ok());
                        match resolved {
                            Some(pos) => pos,
                            None => return false,
                        }
                    }
                    None => type_pos.clone(),
                };
                if !can_satisfy_conditions(&frag_sel.selection_set, &inner_pos, schema) {
                    return false;
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;

    use super::super::test_support;
    use super::*;

    const S1: &str = r#"
        extend schema @link(
          url: "https://specs.apollo.dev/federation/v2.7"
          import: ["@key", "@shareable"]
        )
        type Query { t: T }
        type T @key(fields: "k") {
          k: ID
          x: Int
          xo: Int
          a: A @shareable
        }
        type A { b: Int @shareable, c: Int }
    "#;
    const S2: &str = r#"
        extend schema @link(
          url: "https://specs.apollo.dev/federation/v2.7"
          import: ["@key", "@external", "@requires", "@override", "@shareable"]
        )
        interface I { y: Int }
        type T implements I @key(fields: "k") {
          k: ID
          x: Int @external
          xo: Int @override(from: "S1", label: "pct")
          y: Int @requires(fields: "x")
          a: A @shareable
        }
        type A { b: Int @shareable }
    "#;

    fn space_and_schemas() -> (
        FieldRoutingSearchSpace,
        ValidFederationSchema,
        ValidFederationSchema,
    ) {
        let space = test_support::search_space(&[("S1", S1), ("S2", S2)]);
        let s1 = space
            .query_graph
            .schema_by_source("S1")
            .expect("S1 schema")
            .clone();
        let s2 = space
            .query_graph
            .schema_by_source("S2")
            .expect("S2 schema")
            .clone();
        (space, s1, s2)
    }

    fn composed_schemas() -> (ValidFederationSchema, ValidFederationSchema) {
        let (_, s1, s2) = space_and_schemas();
        (s1, s2)
    }

    fn t_node(space: &FieldRoutingSearchSpace, subgraph: &str) -> NodeIndex {
        test_support::node_for(space, subgraph, "T")
    }

    /// A hidden @requires inside a supertype fragment (which has no
    /// downcast edge at an object node) must still be detected; skipping
    /// the fragment ships the field without its inputs.
    #[test]
    fn requires_detected_through_edgeless_fragment() {
        let (space, _, s2_schema) = space_and_schemas();
        let cond = SelectionSet::parse(s2_schema.clone(), t_pos(&s2_schema), "... on I { y }")
            .expect("conditions parse");
        let s2_t = t_node(&space, "S2");
        assert!(
            space
                .conditions_have_requires(s2_t, &cond)
                .expect("check runs"),
            "y carries @requires even under a supertype fragment with no downcast edge",
        );
    }

    /// A field under a progressive @override label resolves here only when
    /// the label routes here; the schema check must leave that verdict to
    /// the override-aware graph check.
    #[test]
    fn progressive_override_fields_do_not_satisfy_conditions() {
        let (_, _, s2_schema) = space_and_schemas();
        let cond = conditions(&s2_schema, "xo");
        assert!(
            !can_satisfy_conditions(&cond, &t_pos(&s2_schema), &s2_schema),
            "xo is progressively overridden; only the query graph can decide",
        );
    }

    /// The graph-based check must recurse into sub-selections: an edge for
    /// the top-level field is not enough when a nested field has no edge.
    #[test]
    fn graph_resolvability_recurses_into_sub_selections() {
        let (space, s1_schema, _) = space_and_schemas();
        let cond = conditions(&s1_schema, "a { c }");
        let s1_t = t_node(&space, "S1");
        let s2_t = t_node(&space, "S2");
        assert!(
            space
                .conditions_resolvable_at_node(s1_t, &cond)
                .expect("check runs"),
            "S1 resolves a and A.c",
        );
        assert!(
            !space
                .conditions_resolvable_at_node(s2_t, &cond)
                .expect("check runs"),
            "S2 has an edge for a but none for A.c",
        );
    }

    fn t_pos(schema: &ValidFederationSchema) -> CompositeTypeDefinitionPosition {
        schema
            .get_type(&name!("T"))
            .expect("T exists")
            .try_into()
            .expect("T is composite")
    }

    fn conditions(schema: &ValidFederationSchema, text: &str) -> SelectionSet {
        SelectionSet::parse(schema.clone(), t_pos(schema), text).expect("conditions parse")
    }

    /// A field that is defined in the subgraph schema but @external there
    /// cannot satisfy a condition: the subgraph does not resolve it. This
    /// is the common case for @requires condition fields.
    #[test]
    fn external_fields_do_not_satisfy_conditions() {
        let (s1_schema, s2_schema) = composed_schemas();
        // `x` resolves in S1 but is @external in S2.
        let cond = conditions(&s1_schema, "x");
        assert!(can_satisfy_conditions(
            &cond,
            &t_pos(&s1_schema),
            &s1_schema
        ));
        assert!(
            !can_satisfy_conditions(&cond, &t_pos(&s2_schema), &s2_schema),
            "@external x is defined in S2's schema but not resolvable there",
        );
    }

    /// Condition field sets nest; a satisfiable top-level field with an
    /// unsatisfiable sub-selection must not satisfy the condition.
    #[test]
    fn nested_condition_fields_are_checked() {
        let (s1_schema, s2_schema) = composed_schemas();
        // `A.c` exists only in S1; `a { c }` parses there but S2 cannot
        // resolve `c`.
        let cond = conditions(&s1_schema, "a { c }");
        assert!(can_satisfy_conditions(
            &cond,
            &t_pos(&s1_schema),
            &s1_schema
        ));
        assert!(
            !can_satisfy_conditions(&cond, &t_pos(&s2_schema), &s2_schema),
            "a {{ c }} must fail on S2, which has no A.c",
        );
        // The satisfiable nested set still passes.
        let ok = conditions(&s1_schema, "a { b }");
        assert!(can_satisfy_conditions(&ok, &t_pos(&s2_schema), &s2_schema));
    }
}
