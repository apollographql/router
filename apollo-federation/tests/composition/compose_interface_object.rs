use std::collections::HashSet;

use apollo_federation::subgraph::typestate::Subgraph;
use insta::assert_snapshot;
use test_log::test;

use super::ServiceDefinition;
use super::assert_composition_errors;
use super::compose;
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

#[test]
fn interface_with_non_resolvable_key_does_not_require_all_implementations() {
    // subgraphA defines the interface with a resolvable key and all implementations
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

        type C implements I @key(fields: "id") {
          id: ID!
          x: Int
        }
        "#,
    };

    // subgraphB defines the interface with a non-resolvable key but does not
    // define the implementations
    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        interface I @key(fields: "id", resolvable: false) {
          id: ID!
          x: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    // This should not error because a non-resolvable key doesn't require all implementations
    let _supergraph = result.expect(
        "Expected composition to succeed - non-resolvable interface key should not require all implementations"
    );
}

#[test]
fn interface_object_chains_are_not_supported() {
    let s1 = ServiceDefinition {
        name: "S1",
        type_defs: r#"
            type Query {
              i: I1
            }

            interface I1 @key(fields: "id") {
              id: ID!
              data: String!
            }

            interface I2 implements I1 @key(fields: "id") {
              id: ID!
              data: String!
              data2: String!
            }

            type T implements I1 & I2 @key(fields: "id") {
              id: ID!
              data: String!
              data2: String!
            }
        "#,
    };
    let s2 = ServiceDefinition {
        name: "S2",
        type_defs: r#"
            type I1 @interfaceObject @key(fields: "id") {
              id: ID!
              data3: Int
            }
        "#,
    };
    let result = compose_as_fed2_subgraphs(&[s1, s2]);
    assert_composition_errors(
        &result,
        &[(
            "INTERFACE_OBJECT_USAGE_ERROR",
            r#"Interfaces implementing @interfaceObject are not supported: @interfaceObject "I1" is implemented by an interface "I2"."#,
        )],
    );
}

// =============================================================================
// @interfaceObject JOIN FIELD EMISSION TESTS
//
// Verifies that @join__field directives are only emitted when necessary on
// interface fields involving @interfaceObject. Fields shared by ALL subgraphs
// that declare the type should omit @join__field (implicit semantics suffice).
// =============================================================================

#[test]
fn interface_object_shared_key_fields_omit_join_field() {
    // Mirrors the real-world pattern (e.g. ShoppableProduct) where an
    // interface spans two subgraphs — one as a real interface, the other as
    // an @interfaceObject — while additional unrelated subgraphs participate
    // in the composition. Key fields shared by both interface-declaring
    // subgraphs should NOT get @join__field; subgraph-specific fields should.
    //
    // The bystander subgraph is critical: without it, the post-merge
    // remove_redundant_join_fields cleanup would strip the extra directives
    // (since they'd cover all graphs globally), masking the bug.
    let subgraph_a = ServiceDefinition {
        name: "SubgraphA",
        type_defs: r#"
        type Query {
          items: [I]
        }

        interface I @key(fields: "id code") {
          id: ID!
          code: String!
          onlyInA: Int
        }

        type A implements I @key(fields: "id code") {
          id: ID!
          code: String!
          onlyInA: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "SubgraphB",
        type_defs: r#"
        type I @interfaceObject @key(fields: "id code") {
          id: ID!
          code: String!
          onlyInB: Boolean
        }
        "#,
    };

    let bystander = ServiceDefinition {
        name: "Bystander",
        type_defs: r#"
        type Unrelated @key(fields: "id") {
          id: ID!
          name: String
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b, bystander]);
    let supergraph = result.expect("composition should succeed");
    let schema = supergraph.schema().schema();

    let iface = schema
        .types
        .get("I")
        .expect("type I should exist")
        .as_interface()
        .expect("I should be an interface");

    // Key fields present in BOTH subgraphs should NOT have @join__field
    for field_name in ["id", "code"] {
        let field = iface
            .fields
            .get(field_name)
            .expect("field should exist on interface I");
        let join_fields: Vec<_> = field
            .directives
            .iter()
            .filter(|d| d.name == "join__field")
            .collect();
        assert!(
            join_fields.is_empty(),
            "Interface field I.{field_name} is shared by all declaring subgraphs \
             and should NOT have @join__field, but has: {join_fields:?}"
        );
    }

    // Fields present in only ONE subgraph should have @join__field
    for field_name in ["onlyInA", "onlyInB"] {
        let field = iface
            .fields
            .get(field_name)
            .expect("field should exist on interface I");
        let has_join_field = field.directives.iter().any(|d| d.name == "join__field");
        assert!(
            has_join_field,
            "Interface field I.{field_name} exists in only one subgraph \
             and should have @join__field"
        );
    }
}

#[test]
fn interface_object_shared_key_fields_with_three_subgraphs() {
    // Interface declared in SubgraphA with all implementations,
    // @interfaceObject in SubgraphB and SubgraphC, plus a bystander.
    // Key fields shared by all three interface-declaring subgraphs should
    // omit @join__field; subgraph-specific fields should not.
    let subgraph_a = ServiceDefinition {
        name: "SubgraphA",
        type_defs: r#"
        type Query {
          items: [I]
        }

        interface I @key(fields: "id") {
          id: ID!
          aOnly: Int
        }

        type Impl implements I @key(fields: "id") {
          id: ID!
          aOnly: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "SubgraphB",
        type_defs: r#"
        type I @interfaceObject @key(fields: "id") {
          id: ID!
          bOnly: Float
        }
        "#,
    };

    let subgraph_c = ServiceDefinition {
        name: "SubgraphC",
        type_defs: r#"
        type I @interfaceObject @key(fields: "id") {
          id: ID!
          cOnly: Boolean
        }
        "#,
    };

    let bystander = ServiceDefinition {
        name: "Bystander",
        type_defs: r#"
        type Unrelated @key(fields: "id") {
          id: ID!
          name: String
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b, subgraph_c, bystander]);
    let supergraph = result.expect("composition should succeed");
    let schema = supergraph.schema().schema();

    let iface = schema
        .types
        .get("I")
        .expect("type I should exist")
        .as_interface()
        .expect("I should be an interface");

    // `id` is in ALL three subgraphs → should NOT have @join__field
    let id_field = iface.fields.get("id").expect("id field should exist");
    let id_join_fields: Vec<_> = id_field
        .directives
        .iter()
        .filter(|d| d.name == "join__field")
        .collect();
    assert!(
        id_join_fields.is_empty(),
        "Interface field I.id is shared by all declaring subgraphs \
         and should NOT have @join__field, but has: {id_join_fields:?}"
    );

    // Fields in only one subgraph should have exactly 1 @join__field
    for field_name in ["aOnly", "bOnly", "cOnly"] {
        let field = iface
            .fields
            .get(field_name)
            .expect("field should exist on interface I");
        let join_count = field
            .directives
            .iter()
            .filter(|d| d.name == "join__field")
            .count();
        assert_eq!(
            join_count, 1,
            "Interface field I.{field_name} exists in only one subgraph \
             and should have exactly 1 @join__field, but has {join_count}"
        );
    }
}

#[test]
fn interface_object_partial_overlap_needs_join_field() {
    // When a non-key field is present in some-but-not-all subgraphs that
    // declare the interface, @join__field is required even with @interfaceObject.
    // SubgraphA: interface with id + partial, SubgraphB: @interfaceObject with
    // id + partial, SubgraphC: @interfaceObject with id only, plus a bystander.
    let subgraph_a = ServiceDefinition {
        name: "SubgraphA",
        type_defs: r#"
        type Query {
          items: [I]
        }

        interface I @key(fields: "id") {
          id: ID!
          partial: String
        }

        type X implements I @key(fields: "id") {
          id: ID!
          partial: String @shareable
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "SubgraphB",
        type_defs: r#"
        type I @interfaceObject @key(fields: "id") {
          id: ID!
          partial: String @shareable
        }
        "#,
    };

    let subgraph_c = ServiceDefinition {
        name: "SubgraphC",
        type_defs: r#"
        type I @interfaceObject @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let bystander = ServiceDefinition {
        name: "Bystander",
        type_defs: r#"
        type Unrelated @key(fields: "id") {
          id: ID!
          name: String
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b, subgraph_c, bystander]);
    let supergraph = result.expect("composition should succeed");
    let schema = supergraph.schema().schema();

    let iface = schema
        .types
        .get("I")
        .expect("type I should exist")
        .as_interface()
        .expect("I should be an interface");

    // `id` is in all three → no @join__field
    let id_field = iface.fields.get("id").expect("id field should exist");
    let id_join_count = id_field
        .directives
        .iter()
        .filter(|d| d.name == "join__field")
        .count();
    assert_eq!(
        id_join_count, 0,
        "I.id is shared by all subgraphs and should NOT have @join__field"
    );

    // `partial` is in A and B but NOT C → needs @join__field
    let partial_field = iface
        .fields
        .get("partial")
        .expect("partial field should exist");
    let partial_join_count = partial_field
        .directives
        .iter()
        .filter(|d| d.name == "join__field")
        .count();
    assert_eq!(
        partial_join_count, 2,
        "I.partial is in 2 of 3 subgraphs and should have exactly 2 @join__field directives, \
         but has {partial_join_count}"
    );
}

#[test]
fn interface_object_type_kind_mismatch_labels_correctly_when_object_processed_first() {
    // Regression test for a bug where `mismatched_types` and `subgraphs_with_interface_obj`
    // used TypeDefinitionPosition (kind-carrying enum) as keys. When a plain object subgraph
    // was processed before an @interfaceObject subgraph, the merged schema stored Object("I")
    // but the @interfaceObject map was keyed by Interface("I"), causing lookup failures.
    // This produced: (1) wrong labels in TYPE_KIND_MISMATCH (both subgraphs shown as
    // "Object Type"), and (2) a spurious INTERFACE_OBJECT_USAGE_ERROR because the
    // mismatch-suppression check also failed.

    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          iFromA: I
        }

        type I @key(fields: "id") {
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

    // Compose with the plain object subgraph processed first (alphabetical ordering).
    // Before the fix, this produced:
    // 1. TYPE_KIND_MISMATCH with wrong label (both shown as "Object Type")
    // 2. Spurious INTERFACE_OBJECT_USAGE_ERROR
    // After the fix, only TYPE_KIND_MISMATCH with correct labeling is produced.
    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "TYPE_KIND_MISMATCH",
            r#"Type "I" has mismatched kind: it is defined as Object Type in subgraph "subgraphA" but Interface Object Type (Object Type with @interfaceObject) in subgraph "subgraphB""#,
        )],
    );
}
