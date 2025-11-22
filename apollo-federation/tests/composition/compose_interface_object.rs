use std::collections::HashSet;

use apollo_federation::composition::compose;
use apollo_federation::subgraph::typestate::Subgraph;
use insta::assert_snapshot;
use test_log::test;

use super::ServiceDefinition;
use super::assert_composition_errors;
use super::compose_as_fed2_subgraphs;
use super::print_sdl;

// =============================================================================
// @interfaceObject DIRECTIVE TESTS - Tests for @interfaceObject functionality
// =============================================================================

#[test]
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
    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("Expected API schema generation to succeed");
    assert_snapshot!(print_sdl(api_schema.schema()));
}

#[test]
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
    assert_composition_errors(
        &result,
        &[(
            "INTERFACE_OBJECT_USAGE_ERROR",
            r#"Type "I" is declared with @interfaceObject in all the subgraphs in which it is defined (it is defined in subgraphs "subgraphA" and "subgraphB" but should be defined as an interface in at least one subgraph)"#,
        )],
    );
}

#[test]
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
    assert_composition_errors(
        &result,
        &[(
            "TYPE_KIND_MISMATCH",
            r#"Type "I" has mismatched kind: it is defined as Interface Type in subgraph "subgraphA" but Interface Object Type (Object Type with @interfaceObject) in subgraph "subgraphB" and Object Type in subgraph "subgraphC""#,
        )],
    );
}

#[test]
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

    let subgraph_c = ServiceDefinition {
        name: "subgraphC",
        type_defs: r#"
          interface I {
            id: ID!
            x: Int
          }

          type C implements I @key(fields: "id") {
            id: ID!
            x: Int
            w: Int
          }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b, subgraph_c]);
    assert_composition_errors(
        &result,
        &[(
            "INTERFACE_KEY_MISSING_IMPLEMENTATION_TYPE",
            r#"[subgraphA] Interface type "I" has a resolvable key (@key(fields: "id")) in subgraph "subgraphA" but that subgraph is missing some of the supergraph implementation types of "I". Subgraph "subgraphA" should define type "C" (and have it implement "I")."#,
        )],
    );
}

#[test]
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
    assert_composition_errors(
        &result,
        &[(
            "INTERFACE_OBJECT_USAGE_ERROR",
            r#"[subgraphB] Interface type "I" is defined as an @interfaceObject in subgraph "subgraphB" so that subgraph should not define any of the implementation types of "I", but it defines type "A""#,
        )],
    );
}

#[test]
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
    let _supergraph =
        result.expect("Expected composition to succeed with @interfaceObject references");
}

#[test]
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
    let _supergraph = result.expect(
        "Expected composition to succeed - should not error when optimizing unnecessary loops",
    );
}

#[test]
fn interface_object_fed354_repro_failure() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
        type Query {
          error_query: TicketField!
        }

        type User @interfaceObject @key(fields: "id") {
          id: ID!
        }

        interface TicketField {
          id: ID!
          createdBy: User
        }

        type TextTicketField implements TicketField @key(fields: "id") @shareable {
          id: ID!
          createdBy: User
        }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        interface Ticket @key(fields: "id", resolvable: true) {
          id: ID!
        }

        interface User @key(fields: "id", resolvable: true) {
          id: ID!
          requestedTickets: [Ticket!]!
        }

        interface TicketField {
          createdBy: User
          id: ID!
        }

        type TextTicketField implements TicketField @shareable {
          createdBy: User
          id: ID!
        }

        type Customer implements User @key(fields: "id", resolvable: true) @shareable {
          id: ID!
          requestedTickets: [Ticket!]!
        }

        type Agent implements User @key(fields: "id", resolvable: true) @shareable {
          id: ID!
          requestedTickets: [Ticket!]!
        }

        type Question implements Ticket @key(fields: "id", resolvable: true) {
          fields: [TicketField!]!
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
    let _supergraph =
        result.expect("Expected composition to succeed - this is a repro test for issue FED-354");
}

#[test]
fn interface_object_with_inaccessible_field() {
    // Regression test for interface object fields not getting @join__field directives.
    // When an interface has @interfaceObject types in some subgraphs, all fields need
    // @join__field directives to indicate which subgraphs provide them.
    //
    // Setup:
    // - subgraph_a: defines interface with id @inaccessible
    // - subgraph_b: defines interface WITHOUT id field
    // - subgraph_c: @interfaceObject with id in key
    //
    // The bug was that subgraph_a and subgraph_c weren't getting @join__field for id.

    let subgraph_a = r#"
        extend schema
            @link(url: "https://specs.apollo.dev/federation/v2.3", import: ["@key", "@inaccessible"])

        type Query {
            items: [Item]
        }

        interface Item {
            id: ID! @inaccessible
        }

        type Product implements Item @key(fields: "id") {
            id: ID!
            name: String
        }
    "#;

    let subgraph_b = r#"
        extend schema
            @link(url: "https://specs.apollo.dev/federation/v2.3", import: ["@key"])

        interface Item {
            name: String
        }

        type Special implements Item @key(fields: "id") {
            id: ID!
            name: String
        }
    "#;

    let subgraph_c = r#"
        extend schema
            @link(url: "https://specs.apollo.dev/federation/v2.3", import: ["@key", "@interfaceObject"])

        type Item @key(fields: "id") @interfaceObject {
            id: ID!
            extra: String
        }
    "#;

    let parsed_a = Subgraph::parse("subgraph-a", "http://example.com", subgraph_a).unwrap();
    let parsed_b = Subgraph::parse("subgraph-b", "http://example.com", subgraph_b).unwrap();
    let parsed_c = Subgraph::parse("subgraph-c", "http://example.com", subgraph_c).unwrap();

    let supergraph = compose(vec![parsed_a, parsed_b, parsed_c]).unwrap();

    let item_interface = supergraph
        .schema()
        .schema()
        .types
        .get("Item")
        .unwrap()
        .as_interface()
        .unwrap();
    let id_field = item_interface.fields.get("id").unwrap();
    let id_directives: HashSet<_> = id_field.directives.iter().map(|d| d.to_string()).collect();

    assert!(
        id_directives.contains("@join__field(graph: SUBGRAPH_A)"),
        "id field should have @join__field for subgraph-a"
    );
    assert!(
        id_directives.contains("@join__field(graph: SUBGRAPH_C)"),
        "id field should have @join__field for subgraph-c"
    );
}
