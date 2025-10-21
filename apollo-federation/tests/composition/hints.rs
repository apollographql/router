use apollo_federation::supergraph::{CompositionHint, Satisfiable, Supergraph};

use crate::composition::{ServiceDefinition, compose_as_fed2_subgraphs};

/// Helper to assert that a supergraph has no hints
fn assert_no_hints(supergraph: &Supergraph<Satisfiable>) {
    assert!(
        supergraph.hints().is_empty(),
        "Expected no hints but got: {:?}",
        supergraph.hints()
    );
}

/// Helper to assert that a supergraph has a specific hint with matching code and message
fn assert_has_hint(
    supergraph: &Supergraph<Satisfiable>,
    expected_code: &str,
    expected_message: &str,
) {
    let hints = supergraph.hints();
    let expected_code_str = expected_code;

    let matching_hints: Vec<&CompositionHint> = hints
        .iter()
        .filter(|hint| hint.code() == expected_code_str)
        .collect();

    assert!(
        !matching_hints.is_empty(),
        "Expected hint with code '{}' but found hints with codes: {:?}",
        expected_code_str,
        hints.iter().map(|h| h.code()).collect::<Vec<_>>()
    );

    // Find a hint with matching message (can be substring match for flexibility)
    let found_match = matching_hints
        .iter()
        .any(|hint| hint.message().contains(expected_message));

    assert!(
        found_match,
        "Found hint(s) with code '{}' but none contained expected message.\nExpected message containing: {}\nActual messages: {:?}",
        expected_code_str,
        expected_message,
        matching_hints
            .iter()
            .map(|h| h.message())
            .collect::<Vec<_>>()
    );
}

/// Helper to assert exact hint match with code and message
fn assert_has_exact_hint(
    supergraph: &Supergraph<Satisfiable>,
    expected_code: &str,
    expected_message: &str,
) {
    let hints = supergraph.hints();
    let expected_code_str = expected_code;

    let matching_hints: Vec<&CompositionHint> = hints
        .iter()
        .filter(|hint| hint.code() == expected_code_str)
        .collect();

    assert!(
        !matching_hints.is_empty(),
        "Expected hint with code '{}' but found hints with codes: {:?}",
        expected_code_str,
        hints.iter().map(|h| h.code()).collect::<Vec<_>>()
    );

    // Find a hint with exact message match
    let found_match = matching_hints
        .iter()
        .any(|hint| hint.message() == expected_message);

    assert!(
        found_match,
        "Found hint(s) with code '{}' but none had exact expected message.\nExpected message: {}\nActual messages: {:?}",
        expected_code_str,
        expected_message,
        matching_hints
            .iter()
            .map(|h| h.message())
            .collect::<Vec<_>>()
    );
}

// Tests for field/argument type inconsistencies
mod field_type_inconsistencies {
    use super::*;

    #[test]
    fn hint_on_inconsistent_field_type_nullable_vs_non_nullable() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                type T @shareable {
                    f: String
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type T @shareable {
                    f: String!
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_BUT_COMPATIBLE_FIELD_TYPE",
            r#"Type of field "T.f" is inconsistent but compatible across subgraphs: will use type "String" (from subgraph "Subgraph1") in supergraph but "T.f" has subtype "String!" in subgraph "Subgraph2"."#,
        );
    }

    #[test]
    fn hint_on_subtype_mismatch_for_field() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                interface I {
                    v: Int
                }

                type Impl implements I @shareable {
                    v: Int
                }

                type T @shareable {
                    f: I
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                interface I {
                    v: Int
                }

                type Impl implements I @shareable {
                    v: Int
                }

                type T @shareable {
                    f: Impl
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_BUT_COMPATIBLE_FIELD_TYPE",
            r#"Type of field "T.f" is inconsistent but compatible across subgraphs: will use type "I" (from subgraph "Subgraph1") in supergraph but "T.f" has subtype "Impl" in subgraph "Subgraph2"."#,
        );
    }

    #[test]
    fn hint_on_inconsistent_argument_type_nullable_vs_non_nullable() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                type T @shareable {
                    f(a: String!): String
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type T @shareable {
                    f(a: String): String
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_BUT_COMPATIBLE_ARGUMENT_TYPE",
            r#"Type of argument "T.f(a:)" is inconsistent but compatible across subgraphs: will use type "String!" (from subgraph "Subgraph1") in supergraph but "T.f(a:)" has supertype "String" in subgraph "Subgraph2"."#,
        );
    }

    #[test]
    fn hint_on_argument_with_default_value_in_only_some_subgraph() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                type T @shareable {
                    f(a: String = "foo"): String
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type T @shareable {
                    f(a: String): String
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_DEFAULT_VALUE_PRESENCE",
            r#"Argument "T.f(a:)" has a default value in only some subgraphs: will not use a default in the supergraph (there is no default in subgraph "Subgraph2") but "T.f(a:)" has default value "foo" in subgraph "Subgraph1"."#,
        );
    }
}

// Tests for entity consistency
mod entity_consistency {
    use super::*;

    #[test]
    fn hint_on_entity_vs_non_entity_inconsistency() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                type T @key(fields: "k") {
                    k: Int
                    v1: String
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type T @shareable {
                    k: Int
                    v2: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_ENTITY",
            r#"Type "T" is declared as an entity (has a @key applied) in some but not all defining subgraphs: it has no @key in subgraph "Subgraph2" but has some @key in subgraph "Subgraph1"."#,
        );
    }
}

// Tests for value type field presence
mod value_type_fields {
    use super::*;

    #[test]
    fn hint_on_object_field_missing_from_some_subgraphs() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                type T @shareable {
                    a: Int
                    b: Int
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type T @shareable {
                    a: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_OBJECT_VALUE_TYPE_FIELD",
            r#"Field "T.b" of non-entity object type "T" is defined in some but not all subgraphs that define "T": "T.b" is defined in subgraph "Subgraph1" but not in subgraph "Subgraph2"."#,
        );
    }

    #[test]
    fn hint_on_interface_field_missing_from_some_subgraphs() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                interface T {
                    a: Int
                    b: Int
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                interface T {
                    a: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_INTERFACE_VALUE_TYPE_FIELD",
            r#"Field "T.b" of interface type "T" is defined in some but not all subgraphs that define "T": "T.b" is defined in subgraph "Subgraph1" but not in subgraph "Subgraph2"."#,
        );
    }

    #[test]
    fn hint_on_input_object_field_missing_from_some_subgraphs() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                input T {
                    a: Int
                    b: Int
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                input T {
                    a: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_INPUT_OBJECT_FIELD",
            r#"Input object field "b" will not be added to "T" in the supergraph as it does not appear in all subgraphs: it is defined in subgraph "Subgraph1" but not in subgraph "Subgraph2"."#,
        );
    }
}

// Tests for union member inconsistencies
mod union_member_inconsistencies {
    use super::*;

    #[test]
    fn hint_on_union_member_missing_from_some_subgraphs() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                union T = A | B | C

                type A @shareable {
                    a: Int
                }

                type B {
                    b: Int
                }

                type C @shareable {
                    b: Int
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                union T = A | C

                type A @shareable {
                    a: Int
                }

                type C @shareable {
                    b: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_UNION_MEMBER",
            r#"Union type "T" includes member type "B" in some but not all defining subgraphs: "B" is defined in subgraph "Subgraph1" but not in subgraph "Subgraph2"."#,
        );
    }
}

// Tests for enum-related hints
mod enum_hints {
    use super::*;

    #[test]
    fn hint_on_unused_enum_type() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                enum T {
                    V1
                    V2
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                enum T {
                    V1
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "UNUSED_ENUM_TYPE",
            r#"Enum type "T" is defined but unused. It will be included in the supergraph with all the values appearing in any subgraph ("as if" it was only used as an output type)."#,
        );
    }

    #[test]
    fn hints_on_enum_value_of_input_enum_type_not_being_in_all_subgraphs() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a(t: T): Int
                }

                enum T {
                    V1
                    V2
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                enum T {
                    V1
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_ENUM_VALUE_FOR_INPUT_ENUM",
            r#"Value "V2" of enum type "T" will not be part of the supergraph as it is not defined in all the subgraphs defining "T": "V2" is defined in subgraph "Subgraph1" but not in subgraph "Subgraph2"."#,
        );
    }

    #[test]
    fn hints_on_enum_value_of_output_enum_type_not_being_in_all_subgraphs() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    t: T
                }

                enum T {
                    V1
                    V2
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                enum T {
                    V1
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_ENUM_VALUE_FOR_OUTPUT_ENUM",
            "Value \"V2\" of enum type \"T\" has been added to the supergraph but is only defined in a subset of the subgraphs defining \"T\": \"V2\" is defined in subgraph \"Subgraph1\" but not in subgraph \"Subgraph2\".",
        );
    }
}

// Tests for description inconsistencies
mod description_inconsistencies {
    use super::*;

    #[test]
    fn hints_on_inconsistent_description_for_schema_definition() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                """
                Queries to the API
                  - a: gives you a int
                """
                schema {
                    query: Query
                }

                type Query {
                    a: Int
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                """
                Entry point for the API
                """
                schema {
                    query: Query
                }

                type Query {
                    b: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_DESCRIPTION",
            r#"The schema definition has inconsistent descriptions across subgraphs. The supergraph will use description (from subgraph "Subgraph1"):
  """
  Queries to the API
    - a: gives you a int
  """
In subgraph "Subgraph2", the description is:
  """
  Entry point for the API
  """"#,
        );
    }

    #[test]
    fn hints_on_inconsistent_description_for_field() {
        // We make sure the 2nd and 3rd subgraphs have the same description to
        // ensure it's the one that gets picked.
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                type T @shareable {
                    "I don't know what I'm doing"
                    f: Int
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type T @shareable {
                    "Return a super secret integer"
                    f: Int
                }
            "#,
        };

        let subgraph3 = ServiceDefinition {
            name: "Subgraph3",
            type_defs: r#"
                type T @shareable {
                    """
                    Return a super secret integer
                    """
                    f: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2, subgraph3]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_DESCRIPTION",
            r#"Element "T.f" has inconsistent descriptions across subgraphs. The supergraph will use description (from subgraphs "Subgraph2" and "Subgraph3"):
  """
  Return a super secret integer
  """
In subgraph "Subgraph1", the description is:
  """
  I don't know what I'm doing
  """"#,
        );
    }
}

// Tests related to the @override directive
mod override_directive_hints {
    use super::*;

    #[test]
    fn hint_when_from_subgraph_does_not_exist() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                type T @key(fields: "id") {
                    id: Int
                    f: Int @override(from: "Subgraph3")
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type T @key(fields: "id") {
                    id: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "FROM_SUBGRAPH_DOES_NOT_EXIST",
            "Source subgraph \"Subgraph3\" for field \"T.f\" on subgraph \"Subgraph1\" does not exist. Did you mean \"Subgraph1\" or \"Subgraph2\"?",
        );
    }

    #[test]
    fn hint_when_override_directive_can_be_removed() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                type T @key(fields: "id") {
                    id: Int
                    f: Int @override(from: "Subgraph2")
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type T @key(fields: "id") {
                    id: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "OVERRIDE_DIRECTIVE_CAN_BE_REMOVED",
            "Field \"T.f\" on subgraph \"Subgraph1\" no longer exists in the from subgraph. The @override directive can be removed.",
        );
    }

    #[test]
    fn hint_overridden_field_can_be_removed() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                type T @key(fields: "id") {
                    id: Int
                    f: Int @override(from: "Subgraph2")
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type T @key(fields: "id") {
                    id: Int
                    f: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "OVERRIDDEN_FIELD_CAN_BE_REMOVED",
            "Field \"T.f\" on subgraph \"Subgraph2\" is overridden. Consider removing it.",
        );
    }

    #[test]
    fn hint_overridden_field_can_be_made_external() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                type T @key(fields: "id") {
                    id: Int @override(from: "Subgraph2")
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type T @key(fields: "id") {
                    id: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "OVERRIDDEN_FIELD_CAN_BE_REMOVED",
            "Field \"T.id\" on subgraph \"Subgraph2\" is overridden. It is still used in some federation directive(s) (@key, @requires, and/or @provides) and/or to satisfy interface constraint(s), but consider marking it @external explicitly or removing it along with its references.",
        );
    }

    #[test]
    fn hint_when_override_directive_can_be_removed_because_overridden_field_has_been_marked_external()
     {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                type T @key(fields: "id") {
                    id: Int @override(from: "Subgraph2")
                    f: Int
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type T @key(fields: "id") {
                    id: Int @external
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "OVERRIDE_DIRECTIVE_CAN_BE_REMOVED",
            "Field \"T.id\" on subgraph \"Subgraph1\" is not resolved anymore by the from subgraph (it is marked \"@external\" in \"Subgraph2\"). The @override directive can be removed.",
        );
    }

    #[test]
    fn hint_when_progressive_override_migration_is_in_progress() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                type T @key(fields: "id") {
                    id: Int
                    f: Int @override(from: "Subgraph2", label: "percent(1)")
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type T @key(fields: "id") {
                    id: Int
                    f: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        // We should only see one hint related to the progressive override
        assert_eq!(result.hints().len(), 1);
        assert_has_hint(
            &result,
            "OVERRIDE_MIGRATION_IN_PROGRESS",
            "Field \"T.f\" is currently being migrated with progressive @override. Once the migration is complete, remove the field from subgraph \"Subgraph2\".",
        );
    }

    #[test]
    fn hint_when_progressive_override_migration_is_in_progress_for_referenced_field() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: Int
                }

                type T @key(fields: "id") {
                    id: Int @override(from: "Subgraph2", label: "percent(1)")
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type T @key(fields: "id") {
                    id: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        // We should only see one hint related to the progressive override
        assert_eq!(result.hints().len(), 1);
        assert_has_hint(
            &result,
            "OVERRIDE_MIGRATION_IN_PROGRESS",
            "Field \"T.id\" on subgraph \"Subgraph2\" is currently being migrated via progressive @override. It is still used in some federation directive(s) (@key, @requires, and/or @provides) and/or to satisfy interface constraint(s). Once the migration is complete, consider marking it @external explicitly or removing it along with its references.",
        );
    }
}

// Tests for non-repeatable directives used with incompatible arguments
mod non_repeatable_directive_arguments {
    use super::*;

    #[test]
    fn does_not_warn_when_subgraphs_have_the_same_arguments() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: String @shareable @deprecated(reason: "because")
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type Query {
                    a: String @shareable @deprecated(reason: "because")
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_no_hints(&result);
    }

    #[test]
    fn does_not_warn_when_subgraphs_all_use_the_same_argument_defaults() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: String @shareable @deprecated
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type Query {
                    a: String @shareable @deprecated
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_no_hints(&result);
    }

    #[test]
    fn does_not_warn_if_a_subgraph_uses_the_argument_default_and_other_passes_argument_but_it_is_the_default()
     {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: String @shareable @deprecated(reason: "No longer supported")
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type Query {
                    a: String @shareable @deprecated
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_no_hints(&result);
    }

    #[test]
    fn warns_if_a_subgraph_uses_default_argument_but_the_other_uses_different_default() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: String @shareable @deprecated(reason: "bad")
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type Query {
                    a: String @shareable @deprecated
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_NON_REPEATABLE_DIRECTIVE_ARGUMENTS",
            "Non-repeatable directive @deprecated is applied to \"Query.a\" in multiple subgraphs but with incompatible arguments. The supergraph will use arguments {reason: \"bad\"} (from subgraph \"Subgraph1\"), but found no arguments in subgraph \"Subgraph2\".",
        );
    }

    #[test]
    fn warns_if_subgraphs_use_different_argument() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    f: Foo
                }

                scalar Foo @specifiedBy(url: "http://FooSpec.com")
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                scalar Foo @specifiedBy(url: "http://BarSpec.com")
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_NON_REPEATABLE_DIRECTIVE_ARGUMENTS",
            "Non-repeatable directive @specifiedBy is applied to \"Foo\" in multiple subgraphs but with incompatible arguments. The supergraph will use arguments {url: \"http://FooSpec.com\"} (from subgraph \"Subgraph1\"), but found arguments {url: \"http://BarSpec.com\"} in subgraph \"Subgraph2\".",
        );
    }

    #[test]
    fn warns_when_subgraphs_use_different_arguments_but_picks_most_popular_option() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
                type Query {
                    a: String @shareable @deprecated(reason: "because")
                }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
                type Query {
                    a: String @shareable @deprecated(reason: "Replaced by field 'b'")
                }
            "#,
        };

        let subgraph3 = ServiceDefinition {
            name: "Subgraph3",
            type_defs: r#"
                type Query {
                    a: String @shareable @deprecated
                }
            "#,
        };

        let subgraph4 = ServiceDefinition {
            name: "Subgraph4",
            type_defs: r#"
                type Query {
                    a: String @shareable @deprecated(reason: "Replaced by field 'b'")
                }
            "#,
        };

        let result =
            compose_as_fed2_subgraphs(&[subgraph1, subgraph2, subgraph3, subgraph4]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_NON_REPEATABLE_DIRECTIVE_ARGUMENTS",
            "Non-repeatable directive @deprecated is applied to \"Query.a\" in multiple subgraphs but with incompatible arguments. The supergraph will use arguments {reason: \"Replaced by field 'b'\"} (from subgraphs \"Subgraph2\" and \"Subgraph4\"), but found arguments {reason: \"because\"} in subgraph \"Subgraph1\" and no arguments in subgraph \"Subgraph3\".",
        );
    }
}

// Tests for shared field with intersecting but non-equal runtime types
mod shareable_runtime_types {
    use super::*;

    #[test]
    fn hints_for_interfaces() {
        let subgraph_a = ServiceDefinition {
            name: "A",
            type_defs: r#"
                type Query {
                    a: A @shareable
                }

                interface A {
                    x: Int
                }

                type I1 implements A {
                    x: Int
                    i1: Int
                }

                type I2 implements A @shareable {
                    x: Int
                    i1: Int
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "B",
            type_defs: r#"
                type Query {
                    a: A @shareable
                }

                interface A {
                    x: Int
                }

                type I2 implements A @shareable {
                    x: Int
                    i2: Int
                }

                type I3 implements A @shareable {
                    x: Int
                    i3: Int
                }
            "#,
        };

        // Note: hints in this case are generated by post-merge validation, so we need full composition
        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_RUNTIME_TYPES_FOR_SHAREABLE_RETURN",
            r#"For the following supergraph API query:
{
  a {
    ...
  }
}
Shared field "Query.a" return type "A" has different sets of possible runtime types across subgraphs.
Since a shared field must be resolved the same way in all subgraphs, make sure that subgraphs "A" and "B" only resolve "Query.a" to objects of type "I2". In particular:
 - subgraph "A" should never resolve "Query.a" to an object of type "I1";
 - subgraph "B" should never resolve "Query.a" to an object of type "I3".
Otherwise the @shareable contract will be broken."#,
        );
    }

    #[test]
    fn hints_for_unions() {
        let subgraph_a = ServiceDefinition {
            name: "A",
            type_defs: r#"
                type Query {
                    e: E! @shareable
                }

                type E @key(fields: "id") {
                    id: ID!
                    s: U! @shareable
                }

                union U = A | B

                type A @shareable {
                    a: Int
                }

                type B @shareable {
                    b: Int
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "B",
            type_defs: r#"
                type E @key(fields: "id") {
                    id: ID!
                    s: U! @shareable
                }

                union U = A | B | C

                type A @shareable {
                    a: Int
                }

                type B @shareable {
                    b: Int
                }

                type C {
                    c: Int
                }
            "#,
        };

        // Note: hints in this case are generated by post-merge validation, so we need full composition
        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]).unwrap();
        assert_has_hint(
            &result,
            "INCONSISTENT_RUNTIME_TYPES_FOR_SHAREABLE_RETURN",
            r#"For the following supergraph API query:
{
  e {
    s {
      ...
    }
  }
}
Shared field "E.s" return type "U!" has different sets of possible runtime types across subgraphs.
Since a shared field must be resolved the same way in all subgraphs, make sure that subgraphs "A" and "B" only resolve "E.s" to objects of types "A" and "B". In particular:
 - subgraph "B" should never resolve "E.s" to an object of type "C".
Otherwise the @shareable contract will be broken."#,
        );
    }
}

// Tests for implicit federation version upgrades
mod implicit_federation_upgrades {
    use super::*;

    #[test]
    #[ignore = "Federation version upgrade hints not yet implemented in Rust"]
    fn should_hint_that_version_was_upgraded_to_satisfy_directive_requirements() {
        // This test would require implementing federation version upgrade detection
        // which may not be fully implemented in the Rust version yet
    }

    #[test]
    #[ignore = "Federation version upgrade hints not yet implemented in Rust"]
    fn should_show_separate_hints_for_each_upgraded_subgraph() {
        // This test would require implementing federation version upgrade detection
        // which may not be fully implemented in the Rust version yet
    }

    #[test]
    #[ignore = "Federation version upgrade hints not yet implemented in Rust"]
    fn should_not_raise_hints_if_only_upgrade_caused_by_direct_federation_spec_link() {
        // This test would require implementing federation version upgrade detection
        // which may not be fully implemented in the Rust version yet
    }
}
