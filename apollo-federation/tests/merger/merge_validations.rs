// Ported from federation/composition-js/src/__tests__/compose.test.ts
// Original describe block: 'merge validations'

use super::ServiceDefinition;
use super::assert_api_schema_snapshot;
use super::assert_composition_success;
use super::assert_error_contains;
use super::compose_as_fed2_subgraphs;

#[ignore = "until merge implementation completed"]
#[test]
fn errors_when_a_subgraph_is_invalid() {
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
    assert!(result.is_err());

    assert_error_contains(&result, "[subgraphA] Unknown type A");
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_when_a_subgraph_has_a_field_with_an_introspection_reserved_name() {
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
    assert!(result.is_err());

    assert_error_contains(
        &result,
        r#"[subgraphA] Name "__someQuery" must not begin with "__", which is reserved by GraphQL introspection."#,
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_when_the_tag_definition_is_invalid() {
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
    assert!(result.is_err());

    assert_error_contains(
        &result,
        r#"[subgraphA] Invalid definition for directive "@tag": missing required argument "name""#,
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn reject_a_subgraph_named_underscore() {
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
    assert!(result.is_err());

    assert_error_contains(
        &result,
        "[_] Invalid name _ for a subgraph: this name is reserved",
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn reject_if_no_subgraphs_have_a_query() {
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
    assert!(result.is_err());

    assert_error_contains(
        &result,
        "No queries found in any subgraph: a supergraph must have a query root type.",
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn reject_a_type_defined_with_different_kinds_in_different_subgraphs() {
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
    assert!(result.is_err());

    assert_error_contains(
        &result,
        r#"No queries found in any subgraph: a supergraph must have a query root type."#,
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_if_an_external_field_is_not_defined_in_any_other_subgraph() {
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
    assert!(result.is_err());

    assert_error_contains(
        &result,
        r#"Field "A.f" is marked @external on all the subgraphs in which it is listed (subgraph "subgraphB")."#,
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_if_a_mandatory_argument_is_not_in_all_subgraphs() {
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
    assert!(result.is_err());

    assert_error_contains(
        &result,
        r#"Argument "Query.q(a:)" is required in some subgraphs but does not appear in all subgraphs: it is required in subgraph "subgraphA" but does not appear in subgraph "subgraphB""#,
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_if_a_subgraph_argument_is_required_without_arguments_but_that_argument_is_mandatory_in_supergraph()
 {
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
    assert!(result.is_err());

    assert_error_contains(
        &result,
        r#"[subgraphA] On field "T.y", for @requires(fields: "x"): no value provided for argument "arg" of field "T.x" but a value is mandatory as "arg" is required in subgraph "subgraphB""#,
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_if_a_subgraph_argument_is_required_with_an_argument_but_that_argument_is_not_in_supergraph()
 {
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
    assert!(result.is_err());

    assert_error_contains(
        &result,
        r#"[subgraphA] On field "T.y", for @requires(fields: "x(arg: 42)"): cannot provide a value for argument "arg" of field "T.x" as argument "arg" is not defined in subgraph "subgraphB""#,
    );
}

mod post_merge_validations {
    use super::*;
    use crate::merger::assert_error_contains;

    #[ignore = "until merge implementation completed"]
    #[test]
    fn errors_if_a_type_does_not_implement_one_of_its_interface_post_merge() {
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
        assert!(result.is_err());

        assert_error_contains(
            &result,
            r#"Interface field "I.a" is declared in subgraph \"subgraphA\" but type "B", which implements "I" only in subgraph \"subgraphB\" does not have field "a"."#,
        );
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn errors_if_a_type_does_not_implement_one_of_its_interface_post_merge_with_interface_on_interface()
     {
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
        assert!(result.is_err());

        assert_error_contains(
            &result,
            r#"Interface field "J.a" is declared in subgraph \"subgraphA\" but type "B", which implements "J" only in subgraph \"subgraphB\" does not have field "a"."#,
        );
    }
}

#[ignore = "until merge implementation completed"]
#[test]
fn handles_fragments_in_requires_using_inaccessible_types() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query @shareable {
          dummy: Entity
        }

        type Entity @key(fields: "id") {
          id: ID!
          data: Foo
        }

        interface Foo {
          foo: String!
        }

        interface Bar implements Foo {
          foo: String!
          bar: String!
        }

        type Baz implements Foo & Bar @shareable {
          foo: String!
          bar: String!
          baz: String!
        }

        type Qux implements Foo & Bar @shareable {
          foo: String!
          bar: String!
          qux: String!
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query @shareable {
          dummy: Entity
        }

        type Entity @key(fields: "id") {
          id: ID!
          data: Foo @external
          requirer: String! @requires(fields: "data { foo ... on Bar { bar ... on Baz { baz } ... on Qux { qux } } }")
        }

        interface Foo {
          foo: String!
        }

        interface Bar implements Foo {
          foo: String!
          bar: String!
        }

        type Baz implements Foo & Bar @shareable @inaccessible {
          foo: String!
          bar: String!
          baz: String!
        }

        type Qux implements Foo & Bar @shareable {
          foo: String!
          bar: String!
          qux: String!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = assert_composition_success(&result);

    // Verify the composition succeeds and the inaccessible type is properly handled
    assert_api_schema_snapshot(supergraph);
}
