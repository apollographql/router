//! Condition satisfiability: can a set of @requires / @key fields be resolved
//! at a given query graph node?
//!
//! Three flavors of check, each deeper than the last:
//! - `can_satisfy_conditions`: pure schema lookup (field exists, not external).
//! - `conditions_resolvable_at_node`: graph-based, path-sensitive variant.
//! - `conditions_have_requires`: detects @requires on condition edges.

use petgraph::graph::NodeIndex;

use super::FieldRoutingSearchSpace;
use crate::error::FederationError;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::operation::SelectionSet;
use crate::operation::TYPENAME_FIELD;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;

impl FieldRoutingSearchSpace {
    /// Can this subgraph resolve every field in `conditions` at `type_pos`?
    pub(super) fn can_satisfy(
        &self,
        conditions: &SelectionSet,
        type_pos: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> bool {
        can_satisfy_conditions(conditions, type_pos, schema)
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

    /// Can every field in `conditions` be resolved at `node` via outgoing edges?
    pub(super) fn conditions_resolvable_at_node(
        &self,
        node: NodeIndex,
        conditions: &SelectionSet,
    ) -> Result<bool, FederationError> {
        self.walk_conditions_graph(node, conditions, true)
    }

    /// Walk condition fields via graph edges. When `fail_on_unreachable` is
    /// true, returns false if any field or typed fragment lacks an edge
    /// (resolvability check); untyped fragments are transparent. When false,
    /// skips missing fields, walks edgeless typed fragments at this node
    /// (supertype spreads collect here), and returns true if any edge
    /// carries conditions (requires detection).
    fn walk_conditions_graph(
        &self,
        node: NodeIndex,
        conditions: &SelectionSet,
        fail_on_unreachable: bool,
    ) -> Result<bool, FederationError> {
        for selection in conditions.selections.values() {
            match selection {
                crate::operation::Selection::Field(field_sel) => {
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
                    // A field carrying @requires draws data from the entity
                    // representation; it cannot be selected in place.
                    if self
                        .cached_query_graph
                        .query_graph
                        .edge_weight(edge_idx)?
                        .conditions
                        .is_some()
                    {
                        return Ok(!fail_on_unreachable);
                    }
                    if let Some(sub) = &field_sel.selection_set {
                        let (_, tail) = self
                            .cached_query_graph
                            .query_graph
                            .edge_endpoints(edge_idx)?;
                        let sub_result =
                            self.walk_conditions_graph(tail, sub, fail_on_unreachable)?;
                        if sub_result != fail_on_unreachable {
                            return Ok(sub_result);
                        }
                    }
                }
                crate::operation::Selection::InlineFragment(frag_sel) => {
                    // A type-conditioned fragment without a downcast edge
                    // (same-type or supertype spread) collects at this node:
                    // unresolvable for the resolvability check, walked here
                    // for requires detection (over-approximating safely).
                    let target = if frag_sel.inline_fragment.type_condition_position.is_some() {
                        match self
                            .cached_query_graph
                            .edge_for_inline_fragment(node, &frag_sel.inline_fragment)
                        {
                            Some(edge) => {
                                self.cached_query_graph.query_graph.edge_endpoints(edge)?.1
                            }
                            None if fail_on_unreachable => return Ok(false),
                            // FIXME: falling back to `node` when no downcast
                            // edge exists can miss @requires behind the type
                            // condition. Type explosion addresses this.
                            None => node,
                        }
                    } else {
                        node
                    };
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

    /// Do any fields in `conditions` carry @requires at `node`? If so, the conditions cannot be resolved in-place
    /// and need their own entity fetch. Fields without an edge here are ignored.
    pub(super) fn conditions_have_requires(
        &self,
        node: NodeIndex,
        conditions: &SelectionSet,
    ) -> Result<bool, FederationError> {
        self.walk_conditions_graph(node, conditions, false)
    }
}

/// Can this `schema` resolve every field in `conditions` at `type_pos`?
pub(super) fn can_satisfy_conditions(
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
                if needs_graph_check_for_satisfiability(&field_pos, definition, schema) {
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
                // FIXME: this does not compare runtime type sets between
                // the subgraph and supergraph. Type explosion addresses this.
                let inner_pos = match &frag_sel.inline_fragment.type_condition_position {
                    Some(type_cond) => {
                        let Some(pos) = schema
                            .get_type(type_cond.type_name())
                            .ok()
                            .and_then(|ty| CompositeTypeDefinitionPosition::try_from(ty).ok())
                        else {
                            return false;
                        };
                        pos
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

/// Whether the schema-only check cannot determine satisfiability for this
/// field and must defer to the query graph. Covers cases where the field
/// exists in the schema but is not locally resolvable without graph context.
///
/// FIXME: does not detect interface fields whose implementing object fields
/// are @external or carry @requires. The graph-based check catches this, but
/// `can_satisfy || graph_check` short-circuits when this returns false.
fn needs_graph_check_for_satisfiability(
    field_pos: &crate::schema::position::FieldDefinitionPosition,
    definition: &apollo_compiler::schema::FieldDefinition,
    schema: &ValidFederationSchema,
) -> bool {
    schema
        .subgraph_metadata()
        .is_some_and(|meta| meta.external_metadata().is_external(field_pos))
        || has_progressive_override(definition, schema)
}

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

#[cfg(test)]
mod tests {
    use apollo_compiler::name;

    use super::super::test_support;
    use super::*;
    use crate::schema::position::InterfaceTypeDefinitionPosition;
    use crate::schema::position::UnionTypeDefinitionPosition;

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

    /// A condition field that itself carries @requires cannot be resolved
    /// in place; the graph check must reject it.
    #[test]
    fn requires_fields_are_not_resolvable_in_place() {
        let (space, _, s2_schema) = space_and_schemas();
        let cond = conditions(&s2_schema, "y");
        let s2_t = t_node(&space, "S2");
        assert!(
            !space
                .conditions_resolvable_at_node(s2_t, &cond)
                .expect("check runs"),
            "y carries @requires and must not count as resolvable in place",
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

    /// When the query graph has no downcast edge from one abstract type to
    /// another (runtime type intersection < 2), conditions_have_requires
    /// falls back to checking at the parent node and can miss a @requires
    /// that exists only at the concrete implementing type.
    #[test]
    fn requires_missed_through_abstract_type_without_downcast_edge() {
        // I is implemented by A, B, C. J is implemented by only A, so
        // the I-to-J runtime type intersection is {A} (size 1) and the
        // query graph omits the I->J downcast edge. A.w carries @requires.
        const R1: &str = r#"
            extend schema @link(
              url: "https://specs.apollo.dev/federation/v2.7"
              import: ["@key", "@shareable"]
            )
            type Query { i: I }
            interface I { id: ID }
            interface J { id: ID }
            type A implements I & J @key(fields: "id") {
              id: ID
              v: Int @shareable
            }
            type B implements I @key(fields: "id") { id: ID }
            type C implements I @key(fields: "id") { id: ID }
        "#;
        const R2: &str = r#"
            extend schema @link(
              url: "https://specs.apollo.dev/federation/v2.7"
              import: ["@key", "@external", "@requires"]
            )
            interface I { id: ID }
            interface J { id: ID, w: Int }
            type A implements I & J @key(fields: "id") {
              id: ID
              v: Int @external
              w: Int @requires(fields: "v")
            }
            type B implements I @key(fields: "id") { id: ID }
            type C implements I @key(fields: "id") { id: ID }
        "#;
        let space = test_support::search_space(&[("R1", R1), ("R2", R2)]);
        let r2 = space
            .query_graph
            .schema_by_source("R2")
            .expect("R2 schema")
            .clone();
        let i_node = test_support::node_for(&space, "R2", "I");
        let i_pos = CompositeTypeDefinitionPosition::Interface(InterfaceTypeDefinitionPosition {
            type_name: name!("I"),
        });
        let cond =
            SelectionSet::parse(r2.clone(), i_pos, "... on J { w }").expect("conditions parse");

        // A.w has @requires, so this should be true. The fallback to the
        // I node misses it because I has no edge for `w`.
        let result = space
            .conditions_have_requires(i_node, &cond)
            .expect("check runs");
        // Documents the current false negative. Type explosion fixes this.
        assert!(
            !result,
            "known gap: @requires behind abstract type condition is missed without type explosion",
        );
    }

    /// An interface field looks satisfiable in the schema check because it
    /// is declared on the interface, but the implementing object's field is
    /// @external. The schema check does not walk object implementations to
    /// discover this, so it returns a false positive.
    #[test]
    fn interface_field_false_positive_when_object_field_is_external() {
        // S1 owns A.f. S2 declares A.f as @external (for a @requires).
        // Interface I declares f, so the schema check sees I.f as present.
        const P1: &str = r#"
            extend schema @link(
              url: "https://specs.apollo.dev/federation/v2.7"
              import: ["@key"]
            )
            type Query { i: I }
            interface I { id: ID, f: Int }
            type A implements I @key(fields: "id") { id: ID, f: Int }
        "#;
        const P2: &str = r#"
            extend schema @link(
              url: "https://specs.apollo.dev/federation/v2.7"
              import: ["@key", "@external", "@requires"]
            )
            interface I { id: ID, f: Int }
            type A implements I @key(fields: "id") {
              id: ID
              f: Int @external
              g: Int @requires(fields: "f")
            }
        "#;
        let space = test_support::search_space(&[("P1", P1), ("P2", P2)]);
        let p2 = space
            .query_graph
            .schema_by_source("P2")
            .expect("P2 schema")
            .clone();
        let i_pos = CompositeTypeDefinitionPosition::Interface(InterfaceTypeDefinitionPosition {
            type_name: name!("I"),
        });
        let cond = SelectionSet::parse(p2.clone(), i_pos.clone(), "f").expect("conditions parse");

        // The schema check sees I.f as defined and not @external (interface
        // fields cannot be @external), so it returns true. But at runtime,
        // A.f is @external in P2 and cannot be resolved there.
        let result = can_satisfy_conditions(&cond, &i_pos, &p2);
        // Documents the false positive. The graph-based check catches this
        // because the query graph omits the interface field edge when the
        // implementing object field is @external.
        assert!(
            result,
            "known gap: schema check does not see through interface to @external object field",
        );
    }

    /// A type condition's type may have different runtime implementors in
    /// the subgraph vs the supergraph. The schema check only verifies the
    /// type exists in the subgraph, not that the runtime types match.
    #[test]
    fn type_condition_with_mismatched_runtime_types() {
        // In the supergraph, interface J is implemented by A and B.
        // In Q2, J is implemented by only A. A condition `... on J { f }`
        // checked at a union U containing {A, B} in Q2 would miss B.
        const Q1: &str = r#"
            extend schema @link(
              url: "https://specs.apollo.dev/federation/v2.7"
              import: ["@key", "@shareable"]
            )
            type Query { u: U }
            union U = A | B
            interface J { f: Int }
            type A implements J @key(fields: "id") { id: ID, f: Int @shareable }
            type B implements J @key(fields: "id") { id: ID, f: Int @shareable }
        "#;
        const Q2: &str = r#"
            extend schema @link(
              url: "https://specs.apollo.dev/federation/v2.7"
              import: ["@key", "@shareable"]
            )
            union U = A | B
            interface J { f: Int }
            type A implements J @key(fields: "id") { id: ID, f: Int @shareable }
            type B @key(fields: "id") { id: ID, f: Int @shareable }
        "#;
        let space = test_support::search_space(&[("Q1", Q1), ("Q2", Q2)]);
        let q2 = space
            .query_graph
            .schema_by_source("Q2")
            .expect("Q2 schema")
            .clone();
        let u_pos = CompositeTypeDefinitionPosition::Union(UnionTypeDefinitionPosition {
            type_name: name!("U"),
        });

        // Parse against Q1's schema where J has both A and B.
        let q1 = space
            .query_graph
            .schema_by_source("Q1")
            .expect("Q1 schema")
            .clone();
        let cond = SelectionSet::parse(q1.clone(), u_pos.clone(), "... on J { f }")
            .expect("conditions parse");

        // The schema check sees J in Q2 and recurses into it. It returns
        // true because J.f exists. But B does not implement J in Q2, so
        // `... on J` would miss B at runtime without type explosion.
        let result = can_satisfy_conditions(&cond, &u_pos, &q2);
        // Documents the false positive. Type explosion fixes this by
        // intersecting runtime types between subgraph and supergraph.
        assert!(
            result,
            "known gap: schema check does not compare runtime type sets across subgraphs",
        );
    }
}
