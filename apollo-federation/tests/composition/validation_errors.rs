use apollo_federation::error::CompositionError;
use apollo_federation::supergraph::Supergraph;

use super::ServiceDefinition;
use super::compose_as_fed2_subgraphs;

fn error_messages<S>(result: &Result<Supergraph<S>, Vec<CompositionError>>) -> Vec<String> {
    match result {
        Ok(_) => panic!("Expected an error, but got a successful composition"),
        Err(err) => err.iter().map(|e| e.to_string()).collect(),
    }
}
mod requires_tests {
    use super::*;

    #[test]
    // TODO
    #[should_panic(
        expected = "not yet implemented: Implement adding core features to merged schema"
    )]
    fn fails_if_it_cannot_satisfy_a_requires() {
        let subgraph_a = ServiceDefinition {
            name: "A",
            type_defs: r#"
                type Query {
                    a: A
                }

                type A @key(fields: "id") {
                    id: ID!
                    x: Int
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "B",
            type_defs: r#"
                type A @key(fields: "id") {
                    id: ID! @external
                    x: Int @external
                    y: Int @requires(fields: "x")
                    z: Int @requires(fields: "x")
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        let messages = error_messages(&result);
        assert_eq!(
            messages,
            [
                r#"
                The following supergraph API query:
                {
                    a {
                    y
                    }
                }
                cannot be satisfied by the subgraphs because:
                - from subgraph "A": cannot find field "A.y".
                - from subgraph "B": cannot satisfy @require conditions on field "A.y" (please ensure that this is not due to key field "id" being accidentally marked @external).
            "#,
                r#"
                The following supergraph API query:
                {
                    a {
                    z
                    }
                }
                cannot be satisfied by the subgraphs because:
                - from subgraph "A": cannot find field "A.z".
                - from subgraph "B": cannot satisfy @require conditions on field "A.z" (please ensure that this is not due to key field "id" being accidentally marked @external).
            "#,
            ]
        );
    }

    #[test]
    // TODO
    #[should_panic(
        expected = "not yet implemented: Implement adding core features to merged schema"
    )]
    fn fails_if_no_usable_post_requires_keys() {
        let subgraph_a = ServiceDefinition {
            name: "A",
            type_defs: r#"
                type T1 @key(fields: "id") {
                    id: Int!
                    f1: String
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "B",
            type_defs: r#"
                type Query {
                    getT1s: [T1]
                }

                type T1 {
                    id: Int! @shareable
                    f1: String @external
                    f2: T2! @requires(fields: "f1")
                }

                type T2 {
                    a: String
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        let messages = error_messages(&result);
        assert_eq!(
            messages,
            [r#"
                The following supergraph API query:
                {
                    getT1s {
                        f2 {
                            ...
                        }
                    }
                }
                cannot be satisfied by the subgraphs because:
                - from subgraph "B": @require condition on field "T1.f2" can be satisfied but missing usable key on "T1" in subgraph "B" to resume query.
                - from subgraph "A": cannot find field "T1.f2".
            "#]
        );
    }
}

mod non_resolvable_keys_tests {
    use super::*;

    #[test]
    // TODO
    #[should_panic(
        expected = "not yet implemented: Implement adding core features to merged schema"
    )]
    fn fails_if_key_is_declared_non_resolvable_but_would_be_needed() {
        let subgraph_a = ServiceDefinition {
            name: "A",
            type_defs: r#"
                type T @key(fields: "id", resolvable: false) {
                    id: ID!
                    f: String
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "B",
            type_defs: r#"
                type Query {
                    getTs: [T]
                }

                type T @key(fields: "id") {
                    id: ID!
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        let messages = error_messages(&result);
        assert_eq!(
            messages,
            [r#"
                The following supergraph API query:
                {
                    getTs {
                        f
                    }
                }
                cannot be satisfied by the subgraphs because:
                - from subgraph "B":
                    - cannot find field "T.f".
                    - cannot move to subgraph "A", which has field "T.f", because none of the @key defined on type "T" in subgraph "A" are resolvable (they are all declared with their "resolvable" argument set to false).
            "#]
        );
    }
}

mod interface_object_tests {
    use super::*;

    #[test]
    // TODO
    #[should_panic(
        expected = "not yet implemented: Implement adding core features to merged schema"
    )]
    fn fails_on_interface_object_usage_with_missing_key_on_interface() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
                interface I {
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
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        let messages = error_messages(&result);
        assert_eq!(
            messages,
            [
                r#"
                The following supergraph API query:
                {
                    iFromB {
                        ... on A {
                            ...
                        }
                    }
                }
                cannot be satisfied by the subgraphs because:
                - from subgraph "subgraphB": no subgraph can be reached to resolve the implementation type of @interfaceObject type "I".
            "#,
                r#"
                The following supergraph API query:
                {
                    iFromB {
                        ... on B {
                            ...
                        }
                    }
                }
                cannot be satisfied by the subgraphs because:
                - from subgraph "subgraphB": no subgraph can be reached to resolve the implementation type of @interfaceObject type "I".
            "#
            ]
        );
    }

    #[test]
    // TODO
    #[should_panic(
        expected = "not yet implemented: Implement adding core features to merged schema"
    )]
    fn fails_on_interface_object_with_some_unreachable_implementation() {
        let subgraph_a = ServiceDefinition {
            name: "subgraphA",
            type_defs: r#"
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
            "#,
        };

        let subgraph_c = ServiceDefinition {
            name: "subgraphC",
            type_defs: r#"
                type A {
                    z: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b, subgraph_c]);
        let messages = error_messages(&result);
        assert_eq!(
            messages,
            [r#"
                The following supergraph API query:
                {
                    iFromB {
                        ... on A {
                            z
                        }
                    }
                }
                cannot be satisfied by the subgraphs because:
                - from subgraph "subgraphB":
                    - cannot find implementation type "A" (supergraph interface "I" is declared with @interfaceObject in "subgraphB").
                    - cannot move to subgraph "subgraphC", which has field "A.z", because interface "I" is not defined in this subgraph (to jump to "subgraphC", it would need to both define interface "I" and have a @key on it).
                - from subgraph "subgraphA":
                    - cannot find field "A.z".
                    - cannot move to subgraph "subgraphC", which has field "A.z", because type "A" has no @key defined in subgraph "subgraphC".
            "#]
        );
    }
}

// when shared field has non-intersecting runtime types in different subgraphs
mod shared_field_runtime_types_tests {
    use super::*;

    #[test]
    // TODO
    #[should_panic(
        expected = "not yet implemented: Implement adding core features to merged schema"
    )]
    fn errors_for_interfaces() {
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

                type I2 implements A {
                    x: Int
                    i2: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        let messages = error_messages(&result);
        assert_eq!(
            messages,
            [r#"
                For the following supergraph API query:
                {
                    a {
                        ...
                    }
                }
                Shared field "Query.a" return type "A" has a non-intersecting set of possible runtime types across subgraphs. Runtime types in subgraphs are:
                - in subgraph "A", type "I1";
                - in subgraph "B", type "I2".
                This is not allowed as shared fields must resolve the same way in all subgraphs, and that imply at least some common runtime types between the subgraphs.
            "#]
        );
    }

    #[test]
    // TODO
    #[should_panic(
        expected = "not yet implemented: Implement adding core features to merged schema"
    )]
    fn errors_for_unions() {
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

                type A {
                    a: Int
                }

                type B {
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

                union U = C | D

                type C {
                    c: Int
                }

                type D {
                    d: Int
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        let messages = error_messages(&result);
        assert_eq!(
            messages,
            [r#"
                For the following supergraph API query:
                {
                    e {
                        s {
                            ...
                        }
                    }
                }
                Shared field "E.s" return type "U!" has a non-intersecting set of possible runtime types across subgraphs. Runtime types in subgraphs are:
                - in subgraph "A", types "A" and "B";
                - in subgraph "B", types "C" and "D".
                This is not allowed as shared fields must resolve the same way in all subgraphs, and that imply at least some common runtime types between the subgraphs.
            "#]
        );
    }
}

mod other_validation_errors_tests {
    use super::*;

    #[test]
    // TODO
    #[should_panic(
        expected = "not yet implemented: Implement adding core features to merged schema"
    )]
    fn errors_when_max_validation_subgraph_paths_is_exceeded() {
        let subgraph_a = ServiceDefinition {
            name: "A",
            type_defs: r#"
                type Query {
                    a: A
                }

                type A @key(fields: "id") {
                    id: ID!
                    b: B
                    c: C
                    d: D
                }

                type B @key(fields: "id") {
                    id: ID!
                    a: A @shareable
                    b: Int @shareable
                    c: C @shareable
                    d: D @shareable
                }

                type C @key(fields: "id") {
                    id: ID!
                    a: A @shareable
                    b: B @shareable
                    c: Int @shareable
                    d: D @shareable
                }

                type D @key(fields: "id") {
                    id: ID!
                    a: A @shareable
                    b: B @shareable
                    c: C @shareable
                    d: Int @shareable
                }
            "#,
        };

        let subgraph_b = ServiceDefinition {
            name: "B",
            type_defs: r#"
                type B @key(fields: "id") {
                    id: ID!
                    b: Int @shareable
                    c: C @shareable
                    d: D @shareable
                }

                type C @key(fields: "id") {
                    id: ID!
                    b: B @shareable
                    c: Int @shareable
                    d: D @shareable
                }

                type D @key(fields: "id") {
                    id: ID!
                    b: B @shareable
                    c: C @shareable
                    d: Int @shareable
                }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
        let messages = error_messages(&result);
        assert_eq!(
            messages,
            [r#"
                Maximum number of validation subgraph paths exceeded: 12
            "#]
        );
    }
}
