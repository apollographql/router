use super::{assert_composition_errors, compose_as_fed2_subgraphs, ServiceDefinition};

// =============================================================================
// MERGE VALIDATIONS - Tests for validation during the merge phase
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
fn merge_validations_errors_when_a_subgraph_is_invalid() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          a: A
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type A {
          b: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(&result, &[
        ("INVALID_GRAPHQL", "[subgraphA] Unknown type A")
    ]);
}

#[test]
#[ignore = "until merge implementation completed"]
fn merge_validations_errors_when_subgraph_has_introspection_reserved_name() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          __someQuery: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          aValidOne: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(&result, &[
        ("INVALID_GRAPHQL", r#"[subgraphA] Name "__someQuery" must not begin with "__", which is reserved by GraphQL introspection."#)
    ]);
}

#[test]
#[ignore = "until merge implementation completed"]
fn merge_validations_errors_when_tag_definition_is_invalid() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          a: String
        }

        directive @tag on ENUM_VALUE
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type A {
          b: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(&result, &[
        ("DIRECTIVE_DEFINITION_INVALID", r#"[subgraphA] Invalid definition for directive "@tag": missing required argument "name""#)
    ]);
}

#[test]
#[ignore = "until merge implementation completed"]
fn merge_validations_reject_subgraph_named_underscore() {
    let subgraph_a = ServiceDefinition {
        name: "_",
        type_defs: r#"
        type Query {
          a: String
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type A {
          b: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(&result, &[
        ("INVALID_SUBGRAPH_NAME", "[_] Invalid name _ for a subgraph: this name is reserved")
    ]);
}

#[test]
#[ignore = "until merge implementation completed"]
fn merge_validations_reject_if_no_subgraphs_have_query() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type A {
          a: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type B {
          b: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(&result, &[
        ("NO_QUERIES", "No queries found in any subgraph: a supergraph must have a query root type.")
    ]);
}

#[test]
#[ignore = "until merge implementation completed"]
fn merge_validations_reject_type_defined_with_different_kinds() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          q: A
        }

        type A {
          a: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        interface A {
          b: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(&result, &[
        ("TYPE_KIND_MISMATCH", r#"Type "A" has mismatched kind: it is defined as Object Type in subgraph "subgraphA" but Interface Type in subgraph "subgraphB""#)
    ]);
}

#[test]
#[ignore = "until merge implementation completed"]
fn merge_validations_errors_if_external_field_not_defined_elsewhere() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          q: String
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        interface I {
          f: Int
        }

        type A implements I @key(fields: "k") {
          k: ID!
          f: Int @external
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(&result, &[
        ("EXTERNAL_MISSING_ON_BASE", r#"Field "A.f" is marked @external on all the subgraphs in which it is listed (subgraph "subgraphB")."#)
    ]);
}

#[test]
#[ignore = "until merge implementation completed"]
fn merge_validations_errors_if_mandatory_argument_not_in_all_subgraphs() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          q(a: Int!): String @shareable
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          q: String @shareable
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(&result, &[
        ("REQUIRED_ARGUMENT_MISSING_IN_SOME_SUBGRAPH", 
         r#"Argument "Query.q(a:)" is required in some subgraphs but does not appear in all subgraphs: it is required in subgraph "subgraphA" but does not appear in subgraph "subgraphB""#)
    ]);
}

#[test]
#[ignore = "until merge implementation completed"]
fn merge_validations_errors_if_subgraph_required_without_args_but_mandatory_in_supergraph() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          t: T
        }

        type T @key(fields: "id") {
          id: ID!
          x(arg: Int): Int @external
          y: Int @requires(fields: "x")
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type T @key(fields: "id") {
          id: ID!
          x(arg: Int!): Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(&result, &[
        ("REQUIRES_INVALID_FIELDS", 
         r#"[subgraphA] On field "T.y", for @requires(fields: "x"): no value provided for argument "arg" of field "T.x" but a value is mandatory as "arg" is required in subgraph "subgraphB""#)
    ]);
}

#[test]
#[ignore = "until merge implementation completed"]
fn merge_validations_errors_if_subgraph_required_with_arg_not_in_supergraph() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          t: T
        }

        type T @key(fields: "id") {
          id: ID!
          x(arg: Int): Int @external
          y: Int @requires(fields: "x(arg: 42)")
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type T @key(fields: "id") {
          id: ID!
          x: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(&result, &[
        ("REQUIRES_INVALID_FIELDS", 
         r#"[subgraphA] On field "T.y", for @requires(fields: "x(arg: 42)"): cannot provide a value for argument "arg" of field "T.x" as argument "arg" is not defined in subgraph "subgraphB""#)
    ]);
}

// =============================================================================
// POST-MERGE VALIDATIONS - Tests for validation after the merge phase
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
fn post_merge_errors_if_type_does_not_implement_interface_post_merge() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          I: [I!]
        }

        interface I {
          a: Int
        }

        type A implements I {
          a: Int
          b: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        interface I {
          b: Int
        }

        type B implements I {
          b: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(&result, &[
        ("INTERFACE_FIELD_NO_IMPLEM", r#"Interface field "I.a" is declared in subgraph "subgraphA" but type "B", which implements "I" only in subgraph "subgraphB" does not have field "a"."#)
    ]);
}

#[test]
#[ignore = "until merge implementation completed"]
fn post_merge_errors_if_type_does_not_implement_interface_on_interface() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          I: [I!]
        }

        interface I {
          a: Int
        }

        interface J implements I {
          a: Int
          b: Int
        }

        type A implements I & J {
          a: Int
          b: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        interface J {
          b: Int
        }

        type B implements J {
          b: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(&result, &[
        ("INTERFACE_FIELD_NO_IMPLEM", r#"Interface field "J.a" is declared in subgraph "subgraphA" but type "B", which implements "J" only in subgraph "subgraphB" does not have field "a"."#)
    ]);
}

// =============================================================================
// MISC VALIDATIONS - Standalone validation tests
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
fn misc_not_broken_by_similar_field_argument_signatures() {
    // This test validates the case from https://github.com/apollographql/federation/issues/1100 is fixed.
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          t: T
        }

        type T @shareable {
          a(x: String): Int
          b(x: Int): Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type T @shareable {
          a(x: String): Int
          b(x: Int): Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let _supergraph = result.expect("Expected composition to succeed");
}

// =============================================================================
// SATISFIABILITY VALIDATIONS - Tests for satisfiability validation
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]  
fn satisfiability_validation_uses_proper_error_code() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          a: A
        }

        type A @shareable {
          x: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type A @shareable {
          x: Int
          y: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    // This test specifically checks that the error code is SATISFIABILITY_ERROR
    // The exact error message is tested elsewhere
    let errors = result.expect_err("Expected composition to fail due to satisfiability");
    let error_codes: Vec<String> = errors.iter().map(|e| format!("{:?}", e)).collect();
    assert!(error_codes.iter().any(|msg| msg.contains("SATISFIABILITY_ERROR")), 
           "Expected SATISFIABILITY_ERROR but got: {:?}", error_codes);
}

#[test]
#[ignore = "until merge implementation completed"]
fn satisfiability_validation_handles_indirectly_reachable_keys() {
    // This test ensures that a regression introduced by https://github.com/apollographql/federation/pull/1653
    // is properly fixed. All we want to check is that validation succeeds on this example.
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          t: T
        }

        type T @key(fields: "k1") {
          k1: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        # Note: the ordering of the key happens to matter for this to be a proper reproduction of the
        # issue #1653 created.
        type T @key(fields: "k2") @key(fields: "k1") {
          k1: Int
          k2: Int
        }
        "#,
    };

    let subgraph_c = ServiceDefinition {
        name: "subgraphC", 
        type_defs: r#"
        type T @key(fields: "k2") {
          k2: Int
          v: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b, subgraph_c]);
    let _supergraph = result.expect("Expected composition to succeed - satisfiability should pass");
}