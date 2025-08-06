// Ported from federation/composition-js/src/__tests__/compose.test.ts
// Original describe block: '@interfaceObject'

use super::ServiceDefinition;
use super::assert_api_schema_snapshot;
use super::assert_composition_success;
use super::compose_as_fed2_subgraphs;
use super::error_messages;

#[ignore = "until merge implementation completed"]
#[test]
fn composes_valid_interface_object_usages_correctly() {
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
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_if_interface_object_is_used_with_no_corresponding_interface() {
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
    assert!(result.is_err());

    let errors = error_messages(&result);
    assert_eq!(
        errors,
        vec![
            "Type \"I\" is declared with @interfaceObject in all the subgraphs in which is is defined (it is defined in subgraphs \"subgraphA\" and \"subgraphB\" but should be defined as an interface in at least one subgraph)"
        ]
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_if_interface_object_is_missing_in_some_subgraph() {
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
    assert!(result.is_err());

    let errors = error_messages(&result);
    assert_eq!(
        errors,
        vec![
            "Type \"I\" has mismatched kind: it is defined as Interface Type in subgraph \"subgraphA\" but Interface Object Type (Object Type with @interfaceObject) in subgraph \"subgraphB\" and Object Type in subgraph \"subgraphC\""
        ]
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_if_an_interface_has_a_key_but_the_subgraph_do_not_know_all_implementations() {
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
    assert!(result.is_err());

    let errors = error_messages(&result);
    assert_eq!(
        errors,
        vec![
            "[subgraphA] Interface type \"I\" has a resolvable key (@key(fields: \"id\")) in subgraph \"subgraphA\" but that subgraph is missing some of the supergraph implementation types of \"I\". Subgraph \"subgraphA\" should define type \"C\" (and have it implement \"I\")."
        ]
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_if_a_subgraph_defines_both_an_interface_object_and_some_implementations() {
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
    assert!(result.is_err());

    let errors = error_messages(&result);
    assert_eq!(
        errors,
        vec![
            "[subgraphB] Interface type \"I\" is defined as an @interfaceObject in subgraph \"subgraphB\" so that subgraph should not define any of the implementation types of \"I\", but it defines type \"A\""
        ]
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn composes_references_to_interface_object() {
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
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
#[test]
fn fed_354_repro_interface_object_failure() {
    let subgraph_1 = ServiceDefinition {
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

    let subgraph_2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
        interface Ticket @key(fields : "id", resolvable : true) {
            id: ID!
        }

        interface User @key(fields : "id", resolvable : true) {
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

        type Customer implements User @key(fields : "id", resolvable : true) @shareable {
            id: ID!
            requestedTickets: [Ticket!]!
        }

        type Agent implements User @key(fields : "id", resolvable : true) @shareable {
            id: ID!
            requestedTickets: [Ticket!]!
        }

        type Question implements Ticket @key(fields : "id", resolvable : true) {
            fields: [TicketField!]!
            id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_1, subgraph_2]);
    assert_composition_success(&result);
}

#[ignore = "until merge implementation completed"]
#[test]
fn do_not_error_when_optimizing_unnecessary_loops() {
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
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}
