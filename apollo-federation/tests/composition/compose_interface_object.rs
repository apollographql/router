use super::{assert_composition_errors, compose_as_fed2_subgraphs, print_sdl, ServiceDefinition};
use insta::assert_snapshot;

// =============================================================================
// @interfaceObject DIRECTIVE TESTS - Tests for @interfaceObject functionality
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
fn interface_object_composes_valid_usages_correctly() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          iFromA: I
        }

        interface I @key(fields: "id") {
          id: ID!
          x: Int
        }

        type A implements I @key(fields: "id") {
          id: ID!
          x: Int
          w: Int
        }

        type B implements I @key(fields: "id") {
          id: ID!
          x: Int
          z: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          iFromB: I
        }

        type I @interfaceObject @key(fields: "id") {
          id: ID!
          y: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let api_schema = supergraph.to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");
    assert_snapshot!(print_sdl(api_schema.schema()));
}

#[test]
#[ignore = "until merge implementation completed"]
fn interface_object_errors_if_used_with_no_corresponding_interface() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          iFromA: I
        }

        type I @interfaceObject @key(fields: "id") {
          id: ID!
          x: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          iFromB: I
        }

        type I @interfaceObject @key(fields: "id") {
          id: ID!
          y: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(&result, &[
        ("INTERFACE_OBJECT_USAGE_ERROR", r#"Type "I" is declared with @interfaceObject in all the subgraphs in which is is defined (it is defined in subgraphs "subgraphA" and "subgraphB" but should be defined as an interface in at least one subgraph)"#),
    ]);
}

#[test]
#[ignore = "until merge implementation completed"]
fn interface_object_errors_if_missing_in_some_subgraph() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          iFromA: I
        }

        interface I @key(fields: "id") {
          id: ID!
          x: Int
        }

        type A implements I @key(fields: "id") {
          id: ID!
          x: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          iFromB: I
        }

        type I @interfaceObject @key(fields: "id") {
          id: ID!
          y: Int
        }
        "#,
    };

    let subgraph_c = ServiceDefinition {
        name: "subgraphC",
        type_defs: r#"
        type Query {
          iFromC: I
        }

        type I @key(fields: "id") {
          id: ID!
          z: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b, subgraph_c]);
    assert_composition_errors(&result, &[
        ("TYPE_KIND_MISMATCH", r#"Type "I" has mismatched kind: it is defined as Interface Type in subgraph "subgraphA" but Interface Object Type (Object Type with @interfaceObject) in subgraph "subgraphB" and Object Type in subgraph "subgraphC""#),
    ]);
}

#[test]
#[ignore = "until merge implementation completed"]
fn interface_object_errors_if_interface_has_key_but_subgraph_doesnt_know_all_implementations() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          iFromA: I
        }

        interface I @key(fields: "id") {
          id: ID!
          x: Int
        }

        type A implements I @key(fields: "id") {
          id: ID!
          x: Int
        }

        type B implements I @key(fields: "id") {
          id: ID!
          x: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          iFromB: I
        }

        type I @interfaceObject @key(fields: "id") {
          id: ID!
          y: Int
        }

        type A @key(fields: "id") {
          id: ID!
          y: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(&result, &[
        ("INTERFACE_OBJECT_USAGE_ERROR", r#"Interface "I" has a @key in subgraph "subgraphB" but that subgraph does not know all the implementations of "I""#),
    ]);
}

#[test]
#[ignore = "until merge implementation completed"]
fn interface_object_errors_if_subgraph_defines_both_interface_object_and_implementations() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          iFromA: I
        }

        interface I @key(fields: "id") {
          id: ID!
          x: Int
        }

        type A implements I @key(fields: "id") {
          id: ID!
          x: Int
          w: Int
        }

        type B implements I @key(fields: "id") {
          id: ID!
          x: Int
          z: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          iFromB: I
        }

        type I @interfaceObject @key(fields: "id") {
          id: ID!
          y: Int
        }

        type A @key(fields: "id") {
          id: ID!
          y: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(&result, &[
        ("INTERFACE_OBJECT_USAGE_ERROR", r#"[subgraphB] Interface type "I" is defined as an @interfaceObject in subgraph "subgraphB" so that subgraph should not define any of the implementation types of "I", but it defines type "A""#),
    ]);
}

#[test]
#[ignore = "until merge implementation completed"]
fn interface_object_composes_references_to_interface_object() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          i: I @shareable
        }

        interface I @key(fields: "id") {
          id: ID!
          x: Int
        }

        type A implements I @key(fields: "id") {
          id: ID!
          x: Int
        }

        type B implements I @key(fields: "id") {
          id: ID!
          x: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          i: I @shareable
        }

        type I @interfaceObject @key(fields: "id") {
          id: ID!
          y: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let _supergraph = result.expect("Expected composition to succeed with @interfaceObject references");
}

#[test]
#[ignore = "until merge implementation completed"]
fn interface_object_does_not_error_when_optimizing_unnecessary_loops() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type T @key(fields: "id") {
          id: ID!
          i: I
        }

        interface I @key(fields: "id") {
          id: ID!
          x: Int
        }

        type A implements I @key(fields: "id") {
          id: ID!
          x: Int
          u: U
        }

        type B implements I @key(fields: "id") {
          id: ID!
          x: Int
        }

        type U @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          t: T
        }

        type T @key(fields: "id") {
          id: ID!
        }

        type I @interfaceObject @key(fields: "id") {
          id: ID!
        }

        type U @key(fields: "id") {
          id: ID!
          v: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let _supergraph = result.expect("Expected composition to succeed - should not error when optimizing unnecessary loops");
}