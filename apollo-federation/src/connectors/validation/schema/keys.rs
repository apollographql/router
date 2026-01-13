//! Validations to make sure that all `@key` directives in the schema correspond to at least
//! one connector.

use std::fmt;
use std::fmt::Formatter;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Directive;
use apollo_compiler::collections::HashMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::validation::Valid;
use itertools::Itertools;
use shape::Shape;
use shape::ShapeCase;

use crate::connectors::Connector;
use crate::connectors::Namespace;
use crate::connectors::validation::Code;
use crate::connectors::validation::Message;
use crate::connectors::variable::VariableReference;
use crate::link::federation_spec_definition::FEDERATION_FIELDS_ARGUMENT_NAME;

/// Collects keys and entity connectors for comparison and validation.
#[derive(Default)]
pub(crate) struct EntityKeyChecker<'schema> {
    /// Any time we see `type T @key(fields: "f")` (with resolvable: true)
    resolvable_keys: Vec<(FieldSet, &'schema Node<Directive>, &'schema Name)>,
    /// Any time we see either:
    /// - `type Query { t(f: X): T @connect(entity: true) }` (Explicit entity resolver)
    /// - `type T { f: X g: Y @connect(... $this.f ...) }`  (Implicit entity resolver)
    entity_connectors: HashMap<Name, Vec<Valid<FieldSet>>>,
}

impl<'schema> EntityKeyChecker<'schema> {
    pub(crate) fn add_key(&mut self, field_set: &FieldSet, directive: &'schema Node<Directive>) {
        self.resolvable_keys
            .push((field_set.clone(), directive, &directive.name));
    }

    pub(crate) fn add_connector(&mut self, field_set: Valid<FieldSet>, selection_shape: &Shape) {
        let declared_type_name = &field_set.selection_set.ty;

        // Register the connector for the declared type
        self.entity_connectors
            .entry(declared_type_name.clone())
            .or_default()
            .push(field_set.clone());

        // Extract concrete type names from the selection shape's __typename fields.
        // This handles interface-based entity connectors that use ->match to return
        // different concrete types.
        let concrete_types = extract_concrete_typenames(selection_shape);
        for concrete_type_name in concrete_types {
            if &concrete_type_name != declared_type_name {
                self.entity_connectors
                    .entry(concrete_type_name)
                    .or_default()
                    .push(field_set.clone());
            }
        }
    }

    /// For each @key we've seen, check if there's a corresponding entity connector
    /// by semantically comparing the @key field set with the synthesized field set
    /// from the connector's arguments.
    ///
    /// The comparison is done by checking if the @key field set is a subset of the
    /// entity connector's field set. It's not equality because we convert `@external`/
    /// `@requires` fields to keys for simplicity's sake.
    pub(crate) fn check_for_missing_entity_connectors(&self, schema: &Schema) -> Vec<Message> {
        let mut messages = Vec::new();

        for (key, directive, _) in &self.resolvable_keys {
            let for_type = self.entity_connectors.get(&key.selection_set.ty);
            let key_exists = for_type.is_some_and(|connectors| {
                connectors.iter().any(|connector| {
                    // Check if fields match, allowing for interface-implementation type mismatches.
                    // The connector's field_set may have an interface type while the key's field_set
                    // has a concrete implementing type.
                    field_set_fields_are_subset(key, connector)
                })
            });
            if !key_exists {
                messages.push(Message {
                    code: Code::MissingEntityConnector,
                    message: format!(
                        "Entity resolution for `@key(fields: \"{}\")` on `{}` is not implemented by a connector. See https://go.apollo.dev/connectors/entity-rules",
                        directive.argument_by_name(&FEDERATION_FIELDS_ARGUMENT_NAME, schema).ok().and_then(|arg| arg.as_str()).unwrap_or_default(),
                        key.selection_set.ty,
                    ),
                    locations: directive
                        .line_column_range(&schema.sources)
                        .into_iter()
                        .collect(),
                });
            }
        }

        messages
    }
}

impl fmt::Debug for EntityKeyChecker<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("EntityKeyChecker")
            .field(
                "resolvable_keys",
                &self
                    .resolvable_keys
                    .iter()
                    .map(|(fs, _, _)| {
                        format!(
                            "... on {} {}",
                            fs.selection_set.ty,
                            fs.selection_set.serialize().no_indent()
                        )
                    })
                    .collect_vec(),
            )
            .field(
                "entity_connectors",
                &self
                    .entity_connectors
                    .values()
                    .flatten()
                    .map(|fs| {
                        format!(
                            "... on {} {}",
                            fs.selection_set.ty,
                            fs.selection_set.serialize().no_indent()
                        )
                    })
                    .collect_vec(),
            )
            .finish()
    }
}

pub(crate) fn field_set_error(
    variables: &[VariableReference<Namespace>],
    connector: &Connector,
    schema: &Schema,
) -> Message {
    Message {
        code: Code::ConnectorsCannotResolveKey,
        message: format!(
            "Variables used in connector (`{}`) on type `{}` cannot be used to create a valid `@key` directive.",
            variables.iter().join("`, `"),
            connector.id.directive.simple_name()
        ),
        locations: connector
            .name()
            .line_column_range(&schema.sources)
            .into_iter()
            .collect(),
    }
}

fn selection_is_subset(x: &Selection, y: &Selection) -> bool {
    match (x, y) {
        (Selection::Field(x), Selection::Field(y)) => {
            x.name == y.name
                && x.alias == y.alias
                && vec_includes_as_set(
                    &x.selection_set.selections,
                    &y.selection_set.selections,
                    selection_is_subset,
                )
        }
        (Selection::InlineFragment(x), Selection::InlineFragment(y)) => {
            x.type_condition == y.type_condition
                && vec_includes_as_set(
                    &x.selection_set.selections,
                    &y.selection_set.selections,
                    selection_is_subset,
                )
        }
        _ => false,
    }
}

/// Returns true if `inner` is a subset of `outer`.
///
/// Note: apollo_federation::operation::SelectionSet has its own `contains`
/// method I'd love to use, but it requires a ValidFederationSchema, which
/// we don't have during validation. This code can be removed after we rewrite
/// composition in rust and connector validations happen after schema validation
/// and `@link` enrichment.
pub(crate) fn field_set_is_subset(inner: &FieldSet, outer: &FieldSet) -> bool {
    inner.selection_set.ty == outer.selection_set.ty
        && vec_includes_as_set(
            &outer.selection_set.selections,
            &inner.selection_set.selections,
            selection_is_subset,
        )
}

/// Like `field_set_is_subset`, but ignores the type name.
/// This is used when comparing keys on concrete types against connectors that
/// return interface types - the type names won't match but the fields should.
fn field_set_fields_are_subset(inner: &FieldSet, outer: &FieldSet) -> bool {
    vec_includes_as_set(
        &outer.selection_set.selections,
        &inner.selection_set.selections,
        selection_is_subset,
    )
}

// `this` vector includes `other` vector as a set
fn vec_includes_as_set<T>(this: &[T], other: &[T], item_matches: impl Fn(&T, &T) -> bool) -> bool {
    other.iter().all(|other_node| {
        this.iter()
            .any(|this_node| item_matches(this_node, other_node))
    })
}

/// Extract concrete type names from __typename fields in a selection shape.
/// This recursively walks the shape to find all literal __typename values.
fn extract_concrete_typenames(shape: &Shape) -> IndexSet<Name> {
    let mut result = IndexSet::default();
    extract_concrete_typenames_into(shape, false, &mut result);
    result
}

/// Recursively extracts concrete type names from the root-level `__typename` fields in a shape.
///
/// The `in_typename` parameter tracks whether we're currently examining a `__typename` field's
/// value (so we know to extract string literals as type names).
///
/// IMPORTANT: We only extract `__typename` from the root level of the shape, not from nested
/// object fields. A nested `{ author { __typename: "User" } }` doesn't mean this connector
/// can resolve `User` entities - it just returns `User` as a nested field. We DO traverse
/// `One`/`All` at the root level to handle `->match` expressions that return different types.
fn extract_concrete_typenames_into(shape: &Shape, in_typename: bool, result: &mut IndexSet<Name>) {
    match shape.case() {
        // A literal string - extract as type name only if we're examining a __typename value
        ShapeCase::String(Some(s)) => {
            if in_typename && let Ok(name) = Name::new(s.as_str()) {
                result.insert(name);
            }
        }
        // Unknown string - can't extract a concrete type name
        ShapeCase::String(None) => {}
        ShapeCase::Object { fields, .. } => {
            // Check for __typename field - pass in_typename=true for its value
            if let Some(typename_shape) = fields.get("__typename") {
                extract_concrete_typenames_into(typename_shape, true, result);
            }
            // Do NOT recurse into nested fields - we only care about root-level __typename
        }
        // One passes through the in_typename context, so
        // __typename: One<"A", "B"> shapes are allowed (union of possible types).
        ShapeCase::One(shapes) => {
            for shape in shapes.iter() {
                extract_concrete_typenames_into(shape, in_typename, result);
            }
        }
        // All (intersection) needs special handling for __typename:
        // - __typename: All<"A", "A"> (same literal) -> extract "A"
        // - __typename: All<"A", "B"> (different literals) -> INVALID, extract nothing
        // An object can only have one concrete __typename at runtime.
        ShapeCase::All(shapes) => {
            if in_typename {
                // Check if all string literals in the intersection are the same
                let mut seen_literal: Option<&str> = None;
                let mut is_satisfiable = true;
                for shape in shapes.iter() {
                    if let ShapeCase::String(Some(s)) = shape.case() {
                        if let Some(prev) = seen_literal {
                            if prev != s.as_str() {
                                // Different string literals - conflicting __typename!
                                is_satisfiable = false;
                                break;
                            }
                        } else {
                            seen_literal = Some(s.as_str());
                        }
                    }
                }
                // Only extract if the intersection is satisfiable
                if is_satisfiable {
                    for shape in shapes.iter() {
                        extract_concrete_typenames_into(shape, in_typename, result);
                    }
                }
                // If not satisfiable, we intentionally don't extract any typenames
                // from this conflicting intersection
            } else {
                // Not examining __typename, just recurse normally
                for shape in shapes.iter() {
                    extract_concrete_typenames_into(shape, in_typename, result);
                }
            }
        }
        // We traverse arrays because they are automatically mapped in
        // GraphQL, so an array of object shapes can have relevant (root
        // level) __typename fields.
        ShapeCase::Array { prefix, tail } => {
            for shape in prefix.iter() {
                extract_concrete_typenames_into(shape, false, result);
            }
            extract_concrete_typenames_into(tail, false, result);
        }
        ShapeCase::Error(shape::Error { partial, .. }) => {
            // If the error has a partial shape, treat the partial shape
            // as the root shape to extract from.
            if let Some(partial) = partial {
                extract_concrete_typenames_into(partial, in_typename, result);
            }
        }
        // Named types, and leaf types - nothing to extract at the root level
        ShapeCase::Name(_, _) => {}
        ShapeCase::None => {}
        ShapeCase::Bool(_) => {}
        ShapeCase::Int(_) => {}
        ShapeCase::Float => {}
        ShapeCase::Null => {}
        ShapeCase::Unknown => {}
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use apollo_compiler::executable::FieldSet;
    use apollo_compiler::name;
    use apollo_compiler::validation::Valid;
    use rstest::rstest;

    use super::field_set_is_subset;

    fn schema() -> Valid<Schema> {
        Schema::parse_and_validate(
            r#"
        type Query {
            t: T
        }

        type T {
            a: String
            b: B
            c: String
        }

        type B {
            x: String
            y: String
        }
        "#,
            "",
        )
        .unwrap()
    }

    #[rstest]
    #[case("a", "a")]
    #[case("a b { x } c", "a b { x } c")]
    #[case("a", "a c")]
    #[case("b { x }", "b { x y }")]
    fn test_field_set_is_subset(#[case] inner: &str, #[case] outer: &str) {
        let schema = schema();
        let inner = FieldSet::parse_and_validate(&schema, name!(T), inner, "inner").unwrap();
        let outer = FieldSet::parse_and_validate(&schema, name!(T), outer, "outer").unwrap();
        assert!(field_set_is_subset(&inner, &outer));
    }

    #[rstest]
    #[case("a b { x } c", "a")]
    #[case("b { x y }", "b { x }")]
    fn test_field_set_is_not_subset(#[case] inner: &str, #[case] outer: &str) {
        let schema = schema();
        let inner = FieldSet::parse_and_validate(&schema, name!(T), inner, "inner").unwrap();
        let outer = FieldSet::parse_and_validate(&schema, name!(T), outer, "outer").unwrap();
        assert!(!field_set_is_subset(&inner, &outer));
    }

    /// Test case: Union of object shapes via ->match, each with a __typename string literal
    #[test]
    fn test_extract_concrete_typenames_from_match() {
        use crate::connectors::ConnectSpec;
        use crate::connectors::JSONSelection;

        let selection = JSONSelection::parse_with_spec(
            r#"
            id
            ... $(name ?? null)->match(
                [null, { __typename: "Anon" }],
                [@, { __typename: "Named", name: name }],
            )
            "#,
            ConnectSpec::V0_4,
        )
        .unwrap();

        let shape = selection.shape();
        eprintln!("Shape: {}", shape.pretty_print());

        let concrete_types = super::extract_concrete_typenames(&shape);
        eprintln!("Concrete types: {:?}", concrete_types);

        assert!(concrete_types.contains(&name!(Anon)));
        assert!(concrete_types.contains(&name!(Named)));
    }

    /// Test case: __typename field is itself a union of string literals
    #[test]
    fn test_extract_typename_union_of_strings() {
        use shape::Shape;

        // Build shape: { __typename: One(["TypeA", "TypeB"]), id: String }
        let typename_union = Shape::one(
            [
                Shape::string_value("TypeA", []),
                Shape::string_value("TypeB", []),
            ],
            [],
        );
        let shape = Shape::record(
            [
                ("__typename".to_string(), typename_union),
                ("id".to_string(), Shape::string([])),
            ]
            .into(),
            [],
        );

        eprintln!("Shape: {}", shape.pretty_print());

        let concrete_types = super::extract_concrete_typenames(&shape);
        eprintln!("Concrete types: {:?}", concrete_types);

        assert!(concrete_types.contains(&name!(TypeA)));
        assert!(concrete_types.contains(&name!(TypeB)));
        assert_eq!(concrete_types.len(), 2);
    }

    /// Test case: Nested __typename values should NOT be extracted
    #[test]
    fn test_does_not_extract_nested_typename() {
        use shape::Shape;

        // Build shape: { id: String, author: { __typename: "User", id: String } }
        let nested_object = Shape::record(
            [
                ("__typename".to_string(), Shape::string_value("User", [])),
                ("id".to_string(), Shape::string([])),
            ]
            .into(),
            [],
        );
        let shape = Shape::record(
            [
                ("id".to_string(), Shape::string([])),
                ("author".to_string(), nested_object),
            ]
            .into(),
            [],
        );

        eprintln!("Shape: {}", shape.pretty_print());

        let concrete_types = super::extract_concrete_typenames(&shape);
        eprintln!("Concrete types: {:?}", concrete_types);

        // Should NOT extract "User" from the nested author.__typename
        assert!(concrete_types.is_empty());
    }

    /// Test case: Conflicting __typename intersection should NOT be extracted
    ///
    /// When multiple spreads set __typename, the intersection creates
    /// All<"TypeA", "TypeB"> which is unsatisfiable - an object can't
    /// have two different concrete types at once.
    #[test]
    fn test_does_not_extract_conflicting_typename_intersection() {
        use shape::Shape;

        // Build shape: { __typename: All<"Cat", "Dog">, id: String }
        // This represents an impossible object with conflicting typenames
        let conflicting_typename = Shape::all(
            [
                Shape::string_value("Cat", []),
                Shape::string_value("Dog", []),
            ],
            [],
        );
        let shape = Shape::record(
            [
                ("__typename".to_string(), conflicting_typename),
                ("id".to_string(), Shape::string([])),
            ]
            .into(),
            [],
        );

        eprintln!("Shape: {}", shape.pretty_print());

        let concrete_types = super::extract_concrete_typenames(&shape);
        eprintln!("Concrete types: {:?}", concrete_types);

        // Should NOT extract any types from conflicting intersection
        assert!(
            concrete_types.is_empty(),
            "Conflicting __typename intersection should not extract any types"
        );
    }

    /// Test case: Mixed One<> with some valid and some conflicting __typename shapes
    ///
    /// A union where some objects have valid __typename and some have
    /// conflicting intersections - only the valid ones should be extracted.
    #[test]
    fn test_extracts_only_valid_typename_from_mixed_union() {
        use shape::Shape;

        // Valid object: { __typename: "Cat", id: String }
        let valid_cat = Shape::record(
            [
                ("__typename".to_string(), Shape::string_value("Cat", [])),
                ("id".to_string(), Shape::string([])),
            ]
            .into(),
            [],
        );

        // Invalid object: { __typename: All<"Cat", "Dog">, id: String }
        let conflicting = Shape::record(
            [
                (
                    "__typename".to_string(),
                    Shape::all(
                        [
                            Shape::string_value("Cat", []),
                            Shape::string_value("Dog", []),
                        ],
                        [],
                    ),
                ),
                ("id".to_string(), Shape::string([])),
            ]
            .into(),
            [],
        );

        // Valid object: { __typename: "Bird", id: String }
        let valid_bird = Shape::record(
            [
                ("__typename".to_string(), Shape::string_value("Bird", [])),
                ("id".to_string(), Shape::string([])),
            ]
            .into(),
            [],
        );

        // Union of all three
        let shape = Shape::one([valid_cat, conflicting, valid_bird], []);

        eprintln!("Shape: {}", shape.pretty_print());

        let concrete_types = super::extract_concrete_typenames(&shape);
        eprintln!("Concrete types: {:?}", concrete_types);

        // Should extract "Cat" and "Bird" but NOT "Dog" (from the conflicting intersection)
        assert!(concrete_types.contains(&name!(Cat)));
        assert!(concrete_types.contains(&name!(Bird)));
        assert!(!concrete_types.contains(&name!(Dog)));
        assert_eq!(concrete_types.len(), 2);
    }
}
