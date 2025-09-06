use apollo_federation::error::CompositionError;

use crate::composition::ServiceDefinition;
use crate::composition::compose_as_fed2_subgraphs;

#[ignore = "ignored by JS implementation - override on type unsupported"]
#[test]
fn override_whole_type() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
          type Query {
            t: T
          }

          type T @key(fields: "k") @override(from: "Subgraph2") {
            k: ID
            a: Int
            b: Int
          }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
          type T @key(fields: "k") {
            k: ID
            a: Int
            c: Int
          }
        "#,
    };

    let supergraph =
        compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect("composition should succeed");

    let type_t = supergraph
        .schema()
        .schema()
        .types
        .get("T")
        .expect("T exists in the schema");
    assert_eq!(
        type_t.to_string(),
        r#"
          type T
            @join__type(graph: SUBGRAPH1, key: \"k\")
            @join__type(graph: SUBGRAPH2, key: \"k\")
          {
            k: ID
            a: Int
            b: Int
          }
        "#
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn override_single_field() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
            type Query {
                t: T
            }
            type T @key(fields: "k") {
                k: ID
                a: Int @override(from: "Subgraph2")
            }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
            type T @key(fields: "k") {
                k: ID
                a: Int
                b: Int
            }
        "#,
    };

    let supergraph =
        compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect("composition should succeed");

    let type_t = supergraph
        .schema()
        .schema()
        .types
        .get("T")
        .expect("T exists in the schema");
    assert_eq!(
        type_t.to_string(),
        r#"
            type T
              @join__type(graph: SUBGRAPH1, key: \"k\")
              @join__type(graph: SUBGRAPH2, key: \"k\")
            {
              k: ID
              a: Int @join__field(graph: SUBGRAPH1, override: \"Subgraph2\")
              b: Int @join__field(graph: SUBGRAPH2)
            }
        "#
    );

    let api_schema = supergraph
        .to_api_schema(Default::default())
        .expect("valid API schema");
    assert_eq!(
        api_schema.schema().to_string(),
        r#"
          type Query {
            t: T
          }

          type T {
            k: ID
            a: Int
            b: Int
          }
        "#
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn override_field_in_provides() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
            type Query {
              t: T
            }
            type T @key(fields: "k") {
              k: ID
              a: A @shareable
            }
            type A @key(fields: "id") {
              id: ID!
              b: B @override(from: "Subgraph2")
            }
            type B @key(fields: "id") {
              id: ID!
              v: String @shareable
            }
        "#,
    };

    // Note @provides is only allowed on fields that the subgraph does not resolve, but
    // because of nesting, this doesn't equate to all fields in a @provides being
    // external. But it does mean that for an overridden field to be in a @provides,
    // some nesting has to be involved.
    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
            type T @key(fields: "k") {
              k: ID
              a: A @shareable @provides(fields: "b { v }")
            }
            type A @key(fields: "id") {
              id: ID!
              b: B
            }
            type B @key(fields: "id") {
              id: ID!
              v: String @external
            }
        "#,
    };

    let supergraph =
        compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect("composition should succeed");

    let type_a = supergraph
        .schema()
        .schema()
        .types
        .get("A")
        .expect("A exists in the schema");
    assert_eq!(
        type_a.to_string(),
        r#"
          type A
            @join__type(graph: SUBGRAPH1, key: \"id\")
            @join__type(graph: SUBGRAPH2, key: \"id\")
          {
            id: ID!
            b: B @join__field(graph: SUBGRAPH1, override: \"Subgraph2\") @join__field(graph: SUBGRAPH2, usedOverridden: true)
          }
        "#
    );

    // Ensuring the provides is still here.
    let type_t = supergraph
        .schema()
        .schema()
        .types
        .get("T")
        .expect("T exists in the schema");
    assert_eq!(
        type_t.to_string(),
        r#"
          type T
            @join__type(graph: SUBGRAPH1, key: \"k\")
            @join__type(graph: SUBGRAPH2, key: \"k\")
          {
            k: ID
            a: A @join__field(graph: SUBGRAPH1) @join__field(graph: SUBGRAPH2, provides: \"b { v }\")
          }
        "#
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn override_field_in_requires() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
            type Query {
              t: T
            }
            type T @key(fields: "k") {
              k: ID
              a: A @shareable
            }
            type A @key(fields: "id") {
              id: ID!
              b: B @override(from: "Subgraph2")
            }
            type B @key(fields: "id") {
              id: ID!
              v: String @shareable
            }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
            type T @key(fields: "k") {
              k: ID
              a: A @shareable
              x: Int @requires(fields: "a { b { v } }")
            }
            type A @key(fields: "id") {
              id: ID!
              b: B
            }
            type B @key(fields: "id") {
              id: ID!
              v: String @external
            }
        "#,
    };

    let supergraph =
        compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect("composition should succeed");

    // Ensures `A.b` is marked external in Subgraph2 since it's overridden but there is still a @requires mentioning it.
    let type_a = supergraph
        .schema()
        .schema()
        .types
        .get("A")
        .expect("A exists in the schema");
    assert_eq!(
        type_a.to_string(),
        r#"
          type A
            @join__type(graph: SUBGRAPH1, key: \"id\")
            @join__type(graph: SUBGRAPH2, key: \"id\")
          {
            id: ID!
            b: B @join__field(graph: SUBGRAPH1, override: \"Subgraph2\") @join__field(graph: SUBGRAPH2, usedOverridden: true)
          }
        "#
    );

    // Ensuring the requires is still here.
    let type_t = supergraph
        .schema()
        .schema()
        .types
        .get("T")
        .expect("T exists in the schema");
    assert_eq!(
        type_t.to_string(),
        r#"
          type T
            @join__type(graph: SUBGRAPH1, key: \"k\")
            @join__type(graph: SUBGRAPH2, key: \"k\")
          {
            k: ID
            a: A
            x: Int @join__field(graph: SUBGRAPH2, requires: \"a { b { v } }\")
          }
        "#
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn override_field_necessary_for_interface() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
            type Query {
              t: T
            }

            interface I {
              x: Int
            }

            type T implements I @key(fields: "k") {
              k: ID
              x: Int
            }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
            type T @key(fields: "k") {
              k: ID
              x: Int @override(from: "Subgraph1")
            }
        "#,
    };

    let supergraph =
        compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect("composition should succeed");

    // Ensures `T.x` is marked external in Subgraph1 since it's overridden but still required by interface I.
    let type_t = supergraph
        .schema()
        .schema()
        .types
        .get("T")
        .expect("T exists in the schema");
    assert_eq!(
        type_t.to_string(),
        r#"
          type T implements I
            @join__implements(graph: SUBGRAPH1, interface: \"I\")
            @join__type(graph: SUBGRAPH1, key: \"k\")
            @join__type(graph: SUBGRAPH2, key: \"k\")
          {
            k: ID
            x: Int @join__field(graph: SUBGRAPH1, usedOverridden: true) @join__field(graph: SUBGRAPH2, override: \"Subgraph1\")
          }
        "#
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn override_from_self_error() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
            type Query {
              t: T
            }

            type T @key(fields: "k") {
              k: ID
              a: Int @override(from: "Subgraph1")
            }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
            type T @key(fields: "k") {
              k: ID
            }
        "#,
    };

    let errors =
        compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect_err("composition should fail");
    assert_eq!(1, errors.len());
    assert!(
        matches!(errors.first(), Some(CompositionError::OverrideFromSelfError { message }) if message == r#"Source and destination subgraphs "Subgraph1" are the same for overridden field "T.a""#)
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn multiple_override_error() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
            type Query {
              t: T
            }

            type T @key(fields: "k") {
              k: ID
              a: Int @override(from: "Subgraph2")
            }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
            type T @key(fields: "k") {
              k: ID
              a: Int @override(from: "Subgraph1")
            }
        "#,
    };

    let mut errors = compose_as_fed2_subgraphs(&[subgraph1, subgraph2])
        .expect_err("composition should fail")
        .into_iter();
    // port note: below note is an existing comment from JS implementation
    // TODO(JS): This test really should not cause the shareable error to be raised, but to fix it would be a bit of a pain, so punting
    // for now
    assert!(
        matches!(errors.next(), Some(CompositionError::OverrideSourceHasOverride { message }) if message == r#"Field "T.a" on subgraph "Subgraph1" is also marked with directive @override in subgraph "Subgraph2". Only one @override directive is allowed per field."#)
    );
    assert!(
        matches!(errors.next(), Some(CompositionError::OverrideSourceHasOverride { message }) if message == r#"Field "T.a" on subgraph "Subgraph2" is also marked with directive @override in subgraph "Subgraph1". Only one @override directive is allowed per field."#)
    );
    assert!(
        matches!(errors.next(), Some(CompositionError::InvalidFieldSharing { message,.. }) if message == r#"Non-shareable field "T.a" is resolved from multiple subgraphs: it is resolved from subgraphs "Subgraph1" and "Subgraph2" and defined as non-shareable in all of them"#)
    );
    assert!(errors.next().is_none());
}

#[ignore = "ignored by JS implementation - override on type unsupported"]
#[test]
fn override_both_type_and_field_error() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
          type Query {
            t: T
          }

          type T @key(fields: "k") @override(from: "Subgraph2") {
            k: ID
            a: Int @override(from: "Subgraph2")
          }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
          type T @key(fields: "k") {
            k: ID
            a: Int
          }
        "#,
    };

    let errors =
        compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect_err("composition should fail");
    assert_eq!(1, errors.len());
    // unsupported
    // assert!(matches!(errors.first(), Some(CompositionError::OverrideOnBothFieldAndType { message }) if message == r#"Field "T.a" on subgraph "Subgraph1" is marked with @override directive on both the field and the type"#));
}

#[ignore = "until merge implementation completed"]
#[test]
fn override_key_field() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
            type Query {
              t: T
            }

            type T @key(fields: "k") {
              k: ID @override(from: "Subgraph2")
              a: Int
            }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
            type T @key(fields: "k") {
              k: ID
              b: Int
            }
        "#,
    };

    let supergraph =
        compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect("composition was successful");
    let type_t = supergraph
        .schema()
        .schema()
        .types
        .get("T")
        .expect("T exists in the schema");
    assert_eq!(
        type_t.to_string(),
        r#"
          type T
            @join__type(graph: SUBGRAPH1, key: \"k\")
            @join__type(graph: SUBGRAPH2, key: \"k\")
          {
            k: ID @join__field(graph: SUBGRAPH1, override: \"Subgraph2\") @join__field(graph: SUBGRAPH2, usedOverridden: true)
            a: Int @join__field(graph: SUBGRAPH1)
            b: Int @join__field(graph: SUBGRAPH2)
          }
        "#
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn invalid_override_key_field_breaks_composition() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
            type Query {
              t: T
            }
            type T @key(fields: "k") {
              k: ID @override(from: "Subgraph2")
              a: Int
            }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
            type Query {
              otherT: T
            }
            type T @key(fields: "k") {
              k: ID
              b: Int
            }
        "#,
    };

    let mut errors = compose_as_fed2_subgraphs(&[subgraph1, subgraph2])
        .expect_err("composition failed")
        .into_iter();
    assert!(
        matches!(errors.next(), Some(CompositionError::SatisfiabilityError { message })
        if message == r#"
            The following supergraph API query:
              {
                otherT {
                  k
                }
              }
              cannot be satisfied by the subgraphs because:
              - from subgraph "Subgraph2":
                - field "T.k" is not resolvable because it is overridden by subgraph "Subgraph1".
                - cannot move to subgraph "Subgraph1" using @key(fields: "k") of "T", the key field(s) cannot be resolved from subgraph "Subgraph2" (note that some of those key fields are overridden in "Subgraph2").
        "#)
    );
    assert!(
        matches!(errors.next(), Some(CompositionError::SatisfiabilityError { message })
        if message == r#"
            The following supergraph API query:
              {
                otherT {
                  a
                }
              }
              cannot be satisfied by the subgraphs because:
              - from subgraph "Subgraph2":
                - cannot find field "T.a".
                - cannot move to subgraph "Subgraph1" using @key(fields: "k") of "T", the key field(s) cannot be resolved from subgraph "Subgraph2" (note that some of those key fields are overridden in "Subgraph2").
        "#)
    );
    assert!(errors.next().is_none());
}

#[ignore = "until merge implementation completed"]
#[test]
fn override_key_field_with_changed_type_definition() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
            type Query {
              t: T
            }

            type T @key(fields: "k") {
              k: ID
              a: Int @override(from: "Subgraph2")
            }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
            type T @key(fields: "k") {
              k: ID
              a: String
            }
        "#,
    };

    let errors =
        compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect_err("composition failed");
    assert_eq!(1, errors.len());
    assert!(
        matches!(errors.first(), Some(CompositionError::FieldTypeMismatch { message })
        if message == r#"Type of field "T.a" is incompatible across subgraphs: it has type "Int" in subgraph "Subgraph1" but type "String" in subgraph "Subgraph2""#)
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn override_field_that_is_key_in_another_type() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
            type Query {
              t: T
            }

            type T @key(fields: "e { k }") {
              e: E
            }

            type E {
              k: ID @override(from: "Subgraph2")
              a: Int
            }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
            type T @key(fields: "e { k }") {
              e: E
              x: Int
            }

            type E {
              k: ID
              b: Int
            }
        "#,
    };

    let supergraph =
        compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect("composition was successful");
    let type_e = supergraph
        .schema()
        .schema()
        .types
        .get("E")
        .expect("E exists in the schema");
    assert_eq!(
        type_e.to_string(),
        r#"
          type E
            @join__type(graph: SUBGRAPH1)
            @join__type(graph: SUBGRAPH2)
          {
            k: ID @join__field(graph: SUBGRAPH1, override: \"Subgraph2\") @join__field(graph: SUBGRAPH2, usedOverridden: true)
            a: Int @join__field(graph: SUBGRAPH1)
            b: Int @join__field(graph: SUBGRAPH2)
          }
        "#
    );
    let type_t = supergraph
        .schema()
        .schema()
        .types
        .get("T")
        .expect("T exists in the schema");
    assert_eq!(
        type_t.to_string(),
        r#"
          type T
            @join__type(graph: SUBGRAPH1, key: \"e { k }\")
            @join__type(graph: SUBGRAPH2, key: \"e { k }\")
          {
            e: E
            x: Int @join__field(graph: SUBGRAPH2)
          }
        "#
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn override_with_provides_on_overridden_field() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
            type Query {
              t: T
            }

            type T @key(fields: "k") {
              k: ID
              u: U @override(from: "Subgraph2")
            }

            type U @key(fields: "id") {
              id: ID
              name: String
            }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
            type T @key(fields: "k") {
              k: ID
              u: U @provides(fields: "name")
            }

            external type U @key(fields: "id") {
              id: ID
              name: String @external
            }
        "#,
    };

    let errors =
        compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect_err("composition failed");
    assert_eq!(1, errors.len());
    assert!(
        matches!(errors.first(), Some(CompositionError::OverrideCollisionWithAnotherDirective { message })
        if message == r#"@override cannot be used on field "T.u" on subgraph "Subgraph1" since "T.u" on "Subgraph2" is marked with directive "@provides""#)
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn override_with_requires_on_overridden_field() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
            type Query {
              t: T
            }

            type T @key(fields: "k") {
              k: ID
              id: ID
              u: U @override(from: "Subgraph2")
            }

            type U @key(fields: "id") {
              id: ID
            }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
            type T @key(fields: "k") {
              k: ID
              id: ID @external
              u: U @requires(fields: "id")
            }

            extend type U @key(fields: "id") {
              id: ID
            }
        "#,
    };

    let errors =
        compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect_err("composition failed");
    assert_eq!(1, errors.len());
    assert!(
        matches!(errors.first(), Some(CompositionError::OverrideCollisionWithAnotherDirective { message })
        if message == r#"@override cannot be used on field "T.u" on subgraph "Subgraph1" since "T.u" on "Subgraph2" is marked with directive "@requires""#)
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn override_with_external_on_overridden_field() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
            type Query {
              t: T
            }

            type T @key(fields: "k") {
              k: ID @override(from: "Subgraph2") @external
              a: Int
            }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
            type T @key(fields: "k") {
              k: ID
              b: Int
            }
        "#,
    };

    let errors =
        compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect_err("composition failed");
    assert_eq!(1, errors.len());
    assert!(
        matches!(errors.first(), Some(CompositionError::OverrideCollisionWithAnotherDirective { message })
        if message == r#"@override cannot be used on field "T.k" on subgraph "Subgraph1" since "T.k" on "Subgraph1" is marked with directive "@external""#)
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn does_not_allow_override_on_interface_fields() {
    let subgraph1 = ServiceDefinition {
        name: "Subgraph1",
        type_defs: r#"
            type Query {
              i1: I
            }

            interface I {
              k: ID
              a: Int @override(from: "Subgraph2")
            }

            type A implements I @key(fields: "k") {
              k: ID
              a: Int
            }
        "#,
    };

    let subgraph2 = ServiceDefinition {
        name: "Subgraph2",
        type_defs: r#"
            type Query {
              i2: I
            }

            interface I {
              k: ID
              a: Int
            }

            type A implements I @key(fields: "k") {
              k: ID
              a: Int @external
            }
        "#,
    };

    let errors =
        compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect_err("composition failed");
    assert_eq!(1, errors.len());
    assert!(
        matches!(errors.first(), Some(CompositionError::OverrideOnInterface { message })
        if message == r#"@override cannot be used on field "I.a" on subgraph "Subgraph1": @override is not supported on interface type fields."#)
    );
}

// At the moment, we've punted on @override support when interacting with @interfaceObject, so the
// following tests mainly cover the various possible use and show that it currently correctly raise
// some validation errors. We may lift some of those limitation in the future.
mod interface_object {
    use super::*;

    #[ignore = "until merge implementation completed"]
    #[test]
    fn does_not_allow_override_on_interface_object_fields() {
        // We currently rejects @override on fields of an @interfaceObject type. We could lift
        // that limitation in the future, and that would mean such override overrides the field
        // in _all_ the implementations of the target subtype, but that would imply generalizing
        // the handling overridden fields and the override error messages, so we keep that for
        // later.
        // Note that it would be a tad simpler to support @override on an @interfaceObject if
        // the `from` subgraph is also an @interfaceObject, as we can essentially ignore that
        // we have @interfaceObject in such case, but it's a corner case and it's clearer for
        // now to just always reject @override on @interfaceObject.
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                i1: I
              }

              type I @interfaceObject @key(fields: "k") {
                k: ID
                a: Int @override(from: "Subgraph2")
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type Query {
                i2: I
              }

              interface I @key(fields: "k") {
                k: ID
                a: Int
              }

              type A implements I @key(fields: "k") {
                k: ID
                a: Int
              }
            "#,
        };

        let errors =
            compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect_err("composition failed");
        assert_eq!(1, errors.len());
        assert!(
            matches!(errors.first(), Some(CompositionError::OverrideCollisionWithAnotherDirective { message })
            if message == r#"@override is not yet supported on fields of @interfaceObject types: cannot be used on field "I.a" on subgraph "Subgraph1"."#)
        );
    }

    #[ignore = "until merge implementation completed"]
    #[test]
    fn does_not_allow_override_when_overriden_field_is_an_interface_object_field() {
        // We don't allow @override on a concrete type field when the `from` subgraph has
        // an @interfaceObject field "covering" that field. In theory, this could have some
        // use if one wanted to move a field from an @interfaceObject into all its implementations
        // (in another subgraph) but it's also a bit hard to validate/use because we would have
        // to check that all the implementations have an @override for it to be correct and
        // it's unclear how useful that gets.
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                i1: I
              }

              type I @interfaceObject @key(fields: "k") {
                k: ID
                a: Int
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type Query {
                i2: I
              }

              interface I @key(fields: "k") {
                k: ID
                a: Int
              }

              type A implements I @key(fields: "k") {
                k: ID
                a: Int @override(from: "Subgraph1")
              }
            "#,
        };

        let errors =
            compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect_err("composition failed");
        assert_eq!(1, errors.len());
        assert!(
            matches!(errors.first(), Some(CompositionError::OverrideCollisionWithAnotherDirective { message })
            if message == r#"Invalid @override on field "A.a" of subgraph "Subgraph2": source subgraph "Subgraph1" does not have field "A.a" but abstract it in type "I" and overriding abstracted fields is not supported."#)
        );
    }
}

mod progressive_override {
    use super::*;

    #[ignore = "until merge implementation completed"]
    #[test]
    fn verify_override_labels_are_present_in_supergraph() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "k") {
                k: ID
                a: Int @override(from: "Subgraph2", label: "foo")
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type T @key(fields: "k") {
                k: ID
                a: Int
                b: Int
              }
            "#,
        };

        let supergraph =
            compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect("composition was successful");
        let type_t = supergraph
            .schema()
            .schema()
            .types
            .get("T")
            .expect("T exists in the schema");
        assert_eq!(
            type_t.to_string(),
            r#"
                type T
                  @join__type(graph: SUBGRAPH1, key: \"k\")
                  @join__type(graph: SUBGRAPH2, key: \"k\")
                {
                  k: ID
                  a: Int @join__field(graph: SUBGRAPH1, override: \"Subgraph2\", overrideLabel: \"foo\") @join__field(graph: SUBGRAPH2, overrideLabel: \"foo\")
                  b: Int @join__field(graph: SUBGRAPH2)
                }
            "#
        );

        // match api schema
        let api_schema = supergraph
            .to_api_schema(Default::default())
            .expect("valid api schema");
        assert_eq!(
            api_schema.schema().to_string(),
            r#"
            type Query {
              t: T
            }

            type T {
              k: ID
              a: Int
              b: Int
            }
            "#
        );

        // match supergraph schema
        assert_eq!(
            supergraph.schema().schema().to_string(),
            r#"
            schema
              @link(url: \"https://specs.apollo.dev/link/v1.0\")
              @link(url: \"https://specs.apollo.dev/join/v0.5\", for: EXECUTION)
            {
              query: Query
            }

            directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION

            directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

            directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

            directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

            directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

            input join__ContextArgument {
              name: String!
              type: String!
              context: String!
              selection: join__FieldValue!
            }

            scalar join__DirectiveArguments

            scalar join__FieldSet

            scalar join__FieldValue

            enum join__Graph {
              SUBGRAPH1 @join__graph(name: \"Subgraph1\", url: \"https://Subgraph1\")
              SUBGRAPH2 @join__graph(name: \"Subgraph2\", url: \"https://Subgraph2\")
            }

            scalar link__Import

            enum link__Purpose {
              \"\"\"
              `SECURITY` features provide metadata necessary to securely resolve fields.
              \"\"\"
              SECURITY

              \"\"\"
              `EXECUTION` features provide metadata necessary for operation execution.
              \"\"\"
              EXECUTION
            }

            type Query
              @join__type(graph: SUBGRAPH1)
              @join__type(graph: SUBGRAPH2)
            {
              t: T @join__field(graph: SUBGRAPH1)
            }

            type T
              @join__type(graph: SUBGRAPH1, key: \"k\")
              @join__type(graph: SUBGRAPH2, key: \"k\")
            {
              k: ID
              a: Int @join__field(graph: SUBGRAPH1, override: \"Subgraph2\", overrideLabel: \"foo\") @join__field(graph: SUBGRAPH2, overrideLabel: \"foo\")
              b: Int @join__field(graph: SUBGRAPH2)
            }
            "#
        );
    }

    mod label_validation {
        use rstest::rstest;

        use super::*;

        #[rstest]
        #[ignore = "until merge implementation completed"]
        #[case::alphanumeric("abc123")]
        #[ignore = "until merge implementation completed"]
        #[case::alphanumeric_with_special_chars("Z_1-2:3/4.5")]
        fn allows_valid_labels(#[case] label: &str) {
            // labels have to start with a letter and followed with
            // alphanumeric and/or some special _-:./ chars
            let with_valid_label = ServiceDefinition {
                name: "validLabel",
                type_defs: &r#"
                  type Query {
                    t: T
                  }

                  type T @key(fields: "k") {
                    k: ID
                    a: Int
                      @override(from: "overridden", label: "<LABEL>")
                  }
                "#
                .replace("<LABEL>", label),
            };

            let overridden = ServiceDefinition {
                name: "overridden",
                type_defs: r#"
                  type T @key(fields: "k") {
                    k: ID
                    a: Int
                  }
                "#,
            };

            let _ = compose_as_fed2_subgraphs(&[with_valid_label, overridden])
                .expect("composition was successful");
        }

        #[rstest]
        #[ignore = "until merge implementation completed"]
        #[case::starts_with_non_alpha("1_starts-with-non-alpha")]
        #[ignore = "until merge implementation completed"]
        #[case::includes_invalid_chars("includes!@_invalid_chars")]
        fn disallows_invalid_labels(#[case] label: &str) {
            let with_invalid_label = ServiceDefinition {
                name: "invalidLabel",
                type_defs: &r#"
                  type Query {
                    t: T
                  }

                  type T @key(fields: "k") {
                    k: ID
                    a: Int
                      @override(from: "overridden", label: "<LABEL>")
                  }
                "#
                .replace("<LABEL>", label),
            };

            let overridden = ServiceDefinition {
                name: "overridden",
                type_defs: r#"
                  type T @key(fields: "k") {
                    k: ID
                    a: Int
                  }
                "#,
            };

            let errors = compose_as_fed2_subgraphs(&[with_invalid_label, overridden])
                .expect_err("composition failed");
            assert_eq!(1, errors.len());
            assert!(
                matches!(errors.first(), Some(CompositionError::OverrideLabelInvalid { message })
                if *message == format!("Invalid @override label \"{label}\" on field \"T.a\" on subgraph \"invalidLabel\": labels must start with a letter and after that may contain alphanumerics, underscores, minuses, colons, periods, or slashes. Alternatively, labels may be of the form \"percent(x)\" where x is a float between 0-100 inclusive."))
            );
        }

        #[rstest]
        #[ignore = "until merge implementation completed"]
        #[case::half_percent("0.5")]
        #[ignore = "until merge implementation completed"]
        #[case::one("1")]
        #[ignore = "until merge implementation completed"]
        #[case::one_percent("1.0")]
        #[ignore = "until merge implementation completed"]
        #[case::ninety_nine("99.9")]
        fn allows_valid_percent_based_labels(#[case] percent: &str) {
            let with_valid_label = ServiceDefinition {
                name: "validLabel",
                type_defs: &r#"
                  type Query {
                    t: T
                  }

                  type T @key(fields: "k") {
                    k: ID
                    a: Int
                      @override(from: "overridden", label: "<LABEL>")
                  }
                "#
                .replace("<LABEL>", percent),
            };

            let overridden = ServiceDefinition {
                name: "overridden",
                type_defs: r#"
                  type T @key(fields: "k") {
                    k: ID
                    a: Int
                  }
                "#,
            };

            let _ = compose_as_fed2_subgraphs(&[with_valid_label, overridden])
                .expect("composition was successful");
        }

        #[rstest]
        #[ignore = "until merge implementation completed"]
        #[case::point_one(".1")]
        #[ignore = "until merge implementation completed"]
        #[case::one_hundred_and_one("101")]
        #[ignore = "until merge implementation completed"]
        #[case::large_precision("1.1234567879")]
        #[ignore = "until merge implementation completed"]
        #[case::not_a_number("foo")]
        fn disallows_invalid_percent_based_labels(#[case] percent: &str) {
            let with_invalid_label = ServiceDefinition {
                name: "invalidLabel",
                type_defs: &r#"
                  type Query {
                    t: T
                  }

                  type T @key(fields: "k") {
                    k: ID
                    a: Int
                      @override(from: "overridden", label: "percent(<LABEL>)")
                  }
                "#
                .replace("<LABEL>", percent),
            };

            let overridden = ServiceDefinition {
                name: "overridden",
                type_defs: r#"
                  type T @key(fields: "k") {
                    k: ID
                    a: Int
                  }
                "#,
            };

            let errors = compose_as_fed2_subgraphs(&[with_invalid_label, overridden])
                .expect_err("composition failed");
            assert_eq!(1, errors.len());
            assert!(
                matches!(errors.first(), Some(CompositionError::OverrideLabelInvalid { message })
                if *message == format!("Invalid @override label \"percent({percent})\" on field \"T.a\" on subgraph \"invalidLabel\": labels must start with a letter and after that may contain alphanumerics, underscores, minuses, colons, periods, or slashes. Alternatively, labels may be of the form \"percent(x)\" where x is a float between 0-100 inclusive."))
            );
        }
    }

    mod composition_validation {
        use super::*;

        #[ignore = "until merge implementation completed"]
        #[test]
        fn verify_forced_jump_from_s1_to_s2_due_to_override() {
            let subgraph1 = ServiceDefinition {
                name: "Subgraph1",
                type_defs: r#"
                    type Query {
                      t: T
                    }

                    type T @key(fields: "id") {
                      id: ID
                      a: A @override(from: "Subgraph2", label: "foo")
                    }

                    type A @key(fields: "id") {
                      id: ID
                      b: Int
                    }
                "#,
            };

            let subgraph2 = ServiceDefinition {
                name: "Subgraph2",
                type_defs: r#"
                    type T @key(fields: "id") {
                      id: ID
                      a: A
                    }

                    type A @key(fields: "id") {
                      id: ID
                      b: Int @override(from: "Subgraph1", label: "foo")
                    }
            "#,
            };

            let _ = compose_as_fed2_subgraphs(&[subgraph1, subgraph2])
                .expect("composition was successful");
        }

        #[ignore = "until merge implementation completed"]
        #[test]
        fn errors_on_overridden_fields_in_requires_fieldset() {
            let subgraph1 = ServiceDefinition {
                name: "Subgraph1",
                type_defs: r#"
                    type Query {
                      t: T
                    }

                    type T @key(fields: "id") {
                      id: ID
                      a: A @override(from: "Subgraph2", label: "foo")
                    }

                    type A @key(fields: "id") {
                      id: ID
                      b: Int
                      c: Int
                    }
                "#,
            };

            let subgraph2 = ServiceDefinition {
                name: "Subgraph2",
                type_defs: r#"
                    type T @key(fields: "id") {
                      id: ID
                      a: A
                      b: Int @requires(fields: "a { c }")
                    }

                    type A @key(fields: "id") {
                      id: ID
                      b: Int @override(from: "Subgraph1", label: "foo")
                      c: Int @external
                    }
                "#,
            };

            let errors =
                compose_as_fed2_subgraphs(&[subgraph1, subgraph2]).expect_err("composition failed");
            assert_eq!(1, errors.len());
            assert!(
                matches!(errors.first(), Some(CompositionError::SatisfiabilityError { message })
                if message == r#"
                    GraphQLError: The following supergraph API query:
                      {
                        t {
                          b
                        }
                      }
                      cannot be satisfied by the subgraphs because:
                      - from subgraph "Subgraph1": cannot find field "T.b".
                      - from subgraph "Subgraph2": cannot satisfy @require conditions on field "T.b".
                    "#)
            );
        }
    }
}
