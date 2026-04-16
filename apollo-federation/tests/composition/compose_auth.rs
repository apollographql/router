use apollo_compiler::coord;
use apollo_federation::composition::compose;
use apollo_federation::subgraph::typestate::Subgraph;
use insta::assert_snapshot;

use super::ServiceDefinition;
use super::assert_composition_errors;
use super::compose_as_fed2_subgraphs;

// =============================================================================
// @authenticated DIRECTIVE TESTS - Tests for @authenticated functionality
// =============================================================================

#[test]
fn authenticated_comprehensive_locations() {
    let on_object = ServiceDefinition {
        name: "on-object",
        type_defs: r#"
        type Query {
          object: AuthenticatedObject!
        }

        type AuthenticatedObject @authenticated {
          field: Int!
        }
        "#,
    };

    let on_scalar = ServiceDefinition {
        name: "on-scalar",
        type_defs: r#"
        scalar AuthenticatedScalar @authenticated
        "#,
    };

    let on_enum = ServiceDefinition {
        name: "on-enum",
        type_defs: r#"
        enum AuthenticatedEnum @authenticated {
          A
          B
        }
        "#,
    };

    let on_root_field = ServiceDefinition {
        name: "on-root-field",
        type_defs: r#"
        type Query {
          authenticatedRootField: Int! @authenticated
        }
        "#,
    };

    let on_object_field = ServiceDefinition {
        name: "on-object-field",
        type_defs: r#"
        type Query {
          objectWithField: ObjectWithAuthenticatedField!
        }

        type ObjectWithAuthenticatedField {
          field: Int! @authenticated
        }
        "#,
    };

    let on_entity_field = ServiceDefinition {
        name: "on-entity-field",
        type_defs: r#"
        type Query {
          entityWithField: EntityWithAuthenticatedField!
        }

        type EntityWithAuthenticatedField @key(fields: "id") {
          id: ID!
          field: Int! @authenticated
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[
        on_object,
        on_scalar,
        on_enum,
        on_root_field,
        on_object_field,
        on_entity_field,
    ]);
    let supergraph = result.expect("Expected composition to succeed");

    let schema = supergraph.schema().schema();

    // Validate @authenticated is applied to all expected elements:
    // ["AuthenticatedObject", "AuthenticatedScalar", "AuthenticatedEnum", "Query.authenticatedRootField",
    //  "ObjectWithAuthenticatedField.field", "EntityWithAuthenticatedField.field"]

    for coord in [
        coord!(AuthenticatedObject),
        coord!(AuthenticatedScalar),
        coord!(AuthenticatedEnum),
    ] {
        let target = coord.lookup(schema).expect("Target exists");
        let has_auth = target
            .directives()
            .iter()
            .any(|d| d.name == "authenticated");
        assert!(has_auth, "No auth directive found in {target}");
    }
    for coord in [
        coord!(Query.authenticatedRootField),
        coord!(ObjectWithAuthenticatedField.field),
        coord!(EntityWithAuthenticatedField.field),
    ] {
        let target = coord.lookup_field(schema).expect("Target exists");
        let has_auth = target.directives.iter().any(|d| d.name == "authenticated");
        assert!(has_auth, "No auth directive found in {}", target.node);
    }
}

#[test]
fn authenticated_has_correct_definition_in_supergraph() {
    let subgraph_a = ServiceDefinition {
        name: "a",
        type_defs: r#"
        type Query {
          x: Int @authenticated
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a]);
    let supergraph = result.expect("Expected composition to succeed");
    let schema = supergraph.schema().schema();

    // Validate the supergraph has the correct @authenticated spec URL
    let has_authenticated_link = schema.schema_definition.directives.iter().any(|d| {
        d.name == "link"
            && d.arguments.iter().any(|arg| {
                arg.name == "url"
                    && arg
                        .value
                        .to_string()
                        .contains("https://specs.apollo.dev/authenticated/v0.1")
            })
    });
    assert!(
        has_authenticated_link,
        "Expected @link with authenticated spec URL in supergraph"
    );

    // Validate the @authenticated directive definition is properly added
    let authenticated_directive = schema
        .directive_definitions
        .get("authenticated")
        .expect("Expected @authenticated directive definition in supergraph");

    // Compare the directive definition with expected structure
    assert_snapshot!(authenticated_directive, @"directive @authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM");
}

#[test]
fn authenticated_applies_on_types_as_long_as_used_once() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
          type Query {
            a: A
          }
          type A @key(fields: "id") @authenticated {
            id: String!
            a1: String
          }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
          type A @key(fields: "id") {
            id: String!
            a2: String
          }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let schema = supergraph.schema().schema();

    // Validate that @authenticated is applied to type A in the supergraph
    // even though it's only present in subgraphA
    let target = coord!(A)
        .lookup(schema)
        .expect("Type A should exist in supergraph");
    let has_auth = target
        .directives()
        .iter()
        .any(|d| d.name == "authenticated");
    assert!(has_auth, "No auth directive found on {target}");
}

#[test]
fn authenticated_validation_error_on_incompatible_directive_definition() {
    let subgraph_a = ServiceDefinition {
        name: "invalidDefinition",
        type_defs: r#"
          directive @authenticated on ENUM_VALUE

          type Query {
            a: Int
          }

          enum E {
            A @authenticated
          }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a]);
    assert_composition_errors(
        &result,
        &[(
            "DIRECTIVE_DEFINITION_INVALID",
            r#"[invalidDefinition] Invalid definition for directive "@authenticated": "@authenticated" should have locations FIELD_DEFINITION, OBJECT, INTERFACE, SCALAR, ENUM, but found (non-subset) ENUM_VALUE"#,
        )],
    );
}

#[test]
fn authenticated_validation_error_on_invalid_application() {
    let subgraph_a = ServiceDefinition {
        name: "invalidApplication",
        type_defs: r#"
          type Query {
            a: Int
          }

          enum E {
            A @authenticated
          }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a]);
    assert_composition_errors(
        &result,
        &[(
            "INVALID_GRAPHQL",
            r#"[invalidApplication] Error: authenticated directive is not supported for ENUM_VALUE location
   ╭─[ invalidApplication:7:15 ]
   │
 7 │             A @authenticated
   │               ───────┬──────  
   │                      ╰──────── directive cannot be used on ENUM_VALUE
   │ 
   │ Help: the directive must be used in a location that the service has declared support for: FIELD_DEFINITION, OBJECT, INTERFACE, SCALAR, ENUM
───╯
"#,
        )],
    );
}

// =============================================================================
// @requiresScopes DIRECTIVE TESTS - Tests for @requiresScopes functionality
// =============================================================================

#[test]
fn requires_scopes_comprehensive_locations() {
    let on_object = ServiceDefinition {
        name: "on-object",
        type_defs: r#"
        type Query {
          object: ScopedObject!
        }

        type ScopedObject @requiresScopes(scopes: ["object"]) {
          field: Int!
        }
        "#,
    };

    let on_scalar = ServiceDefinition {
        name: "on-scalar",
        type_defs: r#"
        scalar ScopedScalar @requiresScopes(scopes: ["scalar"])
        "#,
    };

    let on_enum = ServiceDefinition {
        name: "on-enum",
        type_defs: r#"
        enum ScopedEnum @requiresScopes(scopes: ["enum"]) {
          A
          B
        }
        "#,
    };

    let on_root_field = ServiceDefinition {
        name: "on-root-field",
        type_defs: r#"
        type Query {
          scopedRootField: Int! @requiresScopes(scopes: ["rootField"])
        }
        "#,
    };

    let on_object_field = ServiceDefinition {
        name: "on-object-field",
        type_defs: r#"
        type Query {
          objectWithField: ObjectWithScopedField!
        }

        type ObjectWithScopedField {
          field: Int! @requiresScopes(scopes: ["objectField"])
        }
        "#,
    };

    let on_entity_field = ServiceDefinition {
        name: "on-entity-field",
        type_defs: r#"
        type Query {
          entityWithField: EntityWithScopedField!
        }

        type EntityWithScopedField @key(fields: "id") {
          id: ID!
          field: Int! @requiresScopes(scopes: ["entityField"])
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[
        on_object,
        on_scalar,
        on_enum,
        on_root_field,
        on_object_field,
        on_entity_field,
    ]);
    let supergraph =
        result.expect("Expected composition to succeed with @requiresScopes on various locations");
    let schema = supergraph.schema().schema();

    // Validate @requiresScopes is applied to all expected elements:
    // ["ScopedObject", "ScopedScalar", "ScopedEnum", "Query.scopedRootField",
    //  "ObjectWithScopedField.field", "EntityWithScopedField.field"]

    for coord in [
        coord!(ScopedObject),
        coord!(ScopedScalar),
        coord!(ScopedEnum),
    ] {
        let target = coord.lookup(schema).expect("Target exists");
        let has_scopes = target
            .directives()
            .iter()
            .any(|d| d.name == "requiresScopes");
        assert!(has_scopes, "No requiresScopes directive found in {target}");
    }
    for coord in [
        coord!(Query.scopedRootField),
        coord!(ObjectWithScopedField.field),
        coord!(EntityWithScopedField.field),
    ] {
        let target = coord.lookup_field(schema).expect("Target exists");
        let has_scopes = target.directives.iter().any(|d| d.name == "requiresScopes");
        assert!(
            has_scopes,
            "No requiresScopes directive found in {}",
            target.node
        );
    }
}

// =============================================================================
// @policy DIRECTIVE TESTS - Tests for @policy functionality
// =============================================================================

#[test]
fn policy_comprehensive_locations() {
    let on_object = ServiceDefinition {
        name: "on-object",
        type_defs: r#"
        type Query {
          object: ScopedObject!
        }

        type ScopedObject @policy(policies: ["object"]) {
          field: Int!
        }
        "#,
    };

    let on_scalar = ServiceDefinition {
        name: "on-scalar",
        type_defs: r#"
        scalar ScopedScalar @policy(policies: ["scalar"])
        "#,
    };

    let on_enum = ServiceDefinition {
        name: "on-enum",
        type_defs: r#"
        enum ScopedEnum @policy(policies: ["enum"]) {
          A
          B
        }
        "#,
    };

    let on_root_field = ServiceDefinition {
        name: "on-root-field",
        type_defs: r#"
        type Query {
          scopedRootField: Int! @policy(policies: ["rootField"])
        }
        "#,
    };

    let on_object_field = ServiceDefinition {
        name: "on-object-field",
        type_defs: r#"
        type Query {
          objectWithField: ObjectWithScopedField!
        }

        type ObjectWithScopedField {
          field: Int! @policy(policies: ["objectField"])
        }
        "#,
    };

    let on_entity_field = ServiceDefinition {
        name: "on-entity-field",
        type_defs: r#"
        type Query {
          entityWithField: EntityWithScopedField!
        }

        type EntityWithScopedField @key(fields: "id") {
          id: ID!
          field: Int! @policy(policies: ["entityField"])
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[
        on_object,
        on_scalar,
        on_enum,
        on_root_field,
        on_object_field,
        on_entity_field,
    ]);
    let supergraph =
        result.expect("Expected composition to succeed with @policy on various locations");
    let schema = supergraph.schema().schema();

    // Validate @policy is applied to all expected elements:
    // ["ScopedObject", "ScopedScalar", "ScopedEnum", "Query.scopedRootField",
    //  "ObjectWithScopedField.field", "EntityWithScopedField.field"]

    for coord in [
        coord!(ScopedObject),
        coord!(ScopedScalar),
        coord!(ScopedEnum),
    ] {
        let target = coord.lookup(schema).expect("Target exists");
        let has_policy = target.directives().iter().any(|d| d.name == "policy");
        assert!(has_policy, "No policy directive found in {target}");
    }
    for coord in [
        coord!(Query.scopedRootField),
        coord!(ObjectWithScopedField.field),
        coord!(EntityWithScopedField.field),
    ] {
        let target = coord.lookup_field(schema).expect("Target exists");
        let has_policy = target.directives.iter().any(|d| d.name == "policy");
        assert!(has_policy, "No policy directive found in {}", target.node);
    }
}

#[test]
fn specs_compose_in_consistent_order() {
    let users_sdl = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@authenticated", "@key", "@policy", "@requiresScopes"])

        type Query {
          users: [User!]! @requiresScopes(scopes: [["admin"]])
          user(id: ID!): User @authenticated
        }

        type User @key(fields: "id") @requiresScopes(scopes: [["user_1"]]) {
          id: ID!
          name: String!
          dob: String! @policy(policies: [["user"]])
        }
    "#;
    let users =
        Subgraph::parse("users", "http://users/graphql", users_sdl).expect("valid users subgraph");

    let address_sdl = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@requiresScopes"])

        type User @key(fields: "id") @requiresScopes(scopes: [["user_2"]]) {
          id: ID!
          address: String
        }
    "#;
    let address = Subgraph::parse("address", "http://address/graphql", address_sdl)
        .expect("valid address subgraph");

    let result = compose(vec![users.clone(), address.clone()]).expect("composes successfully");
    let supergraph_schema = result.schema().schema();
    assert_snapshot!(supergraph_schema);

    let result = compose(vec![address, users]).expect("composes successfully");
    assert_eq!(result.schema().schema(), supergraph_schema);
}

mod transitive_auth {
    use crate::composition::ServiceDefinition;
    use crate::composition::assert_composition_errors;
    use crate::composition::compose_as_fed2_subgraphs;

    #[test]
    fn requires_works_with_explicit_auth() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "id") {
                id: ID
                extra: String @external
                requiresExtra: String @requires(fields: "extra") @authenticated
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type T @key(fields: "id") {
                id: ID
                extra: String @authenticated
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let _supergraph = result.expect("Expected composition to succeed");
    }

    #[test]
    fn requires_works_with_auth_on_the_type() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "id") @policy(policies: [["P1"]]) {
                id: ID
                extra: String @external
                requiresExtra: String @requires(fields: "extra")
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type T @key(fields: "id") {
                id: ID
                extra: String @policy(policies: [["P1"]])
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let _supergraph = result.expect("Expected composition to succeed");
    }

    #[test]
    fn requires_works_with_valid_subset_of_auth() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "id") {
                id: ID
                extra: String @external
                requiresExtra: String @requires(fields: "extra") @requiresScopes(scopes: [["S2", "S1"]])
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type T @key(fields: "id") {
                id: ID
                extra: String @requiresScopes(scopes: [["S1", "S2"], ["S3"]])
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let _supergraph = result.expect("Expected composition to succeed");
    }

    #[test]
    fn requires_works_auth_on_nested_selection() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "id") @authenticated {
                id: ID
                extra: I @external
                requiresExtra: String @requires(fields: "extra { i ... on I1 { i1 } ... on I2 { i2 } }")
                  @requiresScopes(scopes: [["S1", "S2"]]) @policy(policies: [["P1"]])
              }

              interface I {
                i: String
              }

              type I1 implements I @external {
                i: String
                i1: String
              }

              type I2 implements I @external {
                i: String
                i2: Int
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type T @key(fields: "id") {
                id: ID
                extra: I @authenticated
              }

              interface I {
                i: String
              }

              type I1 implements I {
                i: String @requiresScopes(scopes: [["S1"]])
                i1: String @requiresScopes(scopes: [["S2"]])
              }

              type I2 implements I {
                i: String
                i2: Int @policy(policies: [["P1"]])
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let _supergraph = result.expect("Expected composition to succeed");
    }

    #[test]
    fn requires_does_not_work_when_missing_auth() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "id") {
                id: ID
                extra: String @external
                requiresExtra: String @requires(fields: "extra")
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type T @key(fields: "id") {
                id: ID
                extra: String @authenticated
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        assert_composition_errors(
            &result,
            &[(
                "MISSING_TRANSITIVE_AUTH_REQUIREMENTS",
                r#"[Subgraph1] Field "T.requiresExtra" does not specify necessary @authenticated, @requiresScopes and/or @policy auth requirements to access the transitive field "T.extra" data from @requires selection set."#,
            )],
        );
    }

    #[test]
    fn requires_does_not_work_with_invalid_subset_of_auth() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "id") {
                id: ID
                extra: String @external
                requiresExtra: String @requires(fields: "extra") @requiresScopes(scopes: [["S1"]])
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type T @key(fields: "id") {
                id: ID
                extra: String @requiresScopes(scopes: [["S1", "S2"]])
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        assert_composition_errors(
            &result,
            &[(
                "MISSING_TRANSITIVE_AUTH_REQUIREMENTS",
                r#"[Subgraph1] Field "T.requiresExtra" does not specify necessary @authenticated, @requiresScopes and/or @policy auth requirements to access the transitive field "T.extra" data from @requires selection set."#,
            )],
        );
    }

    #[test]
    fn requires_does_not_work_when_missing_auth_on_a_nested_selection() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "id") {
                id: ID
                extra: I @external
                requiresExtra: String @requires(fields: "extra { i ... on I1 { i1 } ... on I2 { i2 } }")
              }

              interface I {
                i: String
              }

              type I1 implements I @external {
                i: String
                i1: String
              }

              type I2 implements I @external {
                i: String
                i2: Int
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type T @key(fields: "id") {
                id: ID
                extra: I
              }

              interface I {
                i: String
              }

              type I1 implements I {
                i: String
                i1: String
              }

              type I2 implements I {
                i: String
                i2: Int @policy(policies: [["P1"]])
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        assert_composition_errors(
            &result,
            &[(
                "MISSING_TRANSITIVE_AUTH_REQUIREMENTS",
                r#"[Subgraph1] Field "T.requiresExtra" does not specify necessary @authenticated, @requiresScopes and/or @policy auth requirements to access the transitive field "I2.i2" data from @requires selection set."#,
            )],
        );
    }

    #[test]
    fn requires_does_not_work_when_missing_explicit_auth_on_an_interface_field_selection() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "id") {
                id: ID
                extra: I @external
                requiresExtra: String @requires(fields: "extra { i }")
              }

              interface I {
                i: String
              }

              type I1 implements I @external {
                i: String
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type T @key(fields: "id") {
                id: ID
                extra: I
              }

              interface I {
                i: String
              }

              type I1 implements I {
                i: String @requiresScopes(scopes: [["S1"]])
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        assert_composition_errors(
            &result,
            &[(
                "MISSING_TRANSITIVE_AUTH_REQUIREMENTS",
                r#"[Subgraph1] Field "T.requiresExtra" does not specify necessary @authenticated, @requiresScopes and/or @policy auth requirements to access the transitive field "I.i" data from @requires selection set."#,
            )],
        );
    }

    #[test]
    fn requires_does_not_work_when_missing_inherited_auth_on_an_interface_field_selection() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "id") {
                id: ID
                extra: I @external
                requiresExtra: String @requires(fields: "extra { i }")
              }

              interface I {
                i: String
              }

              type I1 implements I @external {
                i: String
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type T @key(fields: "id") {
                id: ID
                extra: I
              }

              interface I {
                i: String
              }

              type I1 implements I @authenticated {
                i: String
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        assert_composition_errors(
            &result,
            &[(
                "MISSING_TRANSITIVE_AUTH_REQUIREMENTS",
                r#"[Subgraph1] Field "T.requiresExtra" does not specify necessary @authenticated, @requiresScopes and/or @policy auth requirements to access the transitive field "T.extra" data from @requires selection set."#,
            )],
        );
    }

    #[test]
    fn requires_does_not_work_when_missing_auth_on_type_condition_in_a_field_selection() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "id") {
                id: ID
                extra: I @external
                requiresExtra: String @requires(fields: "extra { ... on I1 { i1 } ... on I2 { i2 }}")
              }

              interface I {
                i: String
              }

              type I1 implements I @external {
                i: String
                i1: Int
              }

              type I2 implements I @external {
                i: String
                i2: String
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type T @key(fields: "id") {
                id: ID
                extra: I
              }

              interface I {
                i: String
              }

              type I1 implements I @requiresScopes(scopes: [["S1"]]) {
                i: String
                i1: Int
              }

              type I2 implements I {
                i: String
                i2: String
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        assert_composition_errors(
            &result,
            &[(
                "MISSING_TRANSITIVE_AUTH_REQUIREMENTS",
                r#"[Subgraph1] Field "T.requiresExtra" does not specify necessary @authenticated, @requiresScopes and/or @policy auth requirements to access the transitive field "T.extra" data from @requires selection set."#,
            )],
        );
    }

    #[test]
    fn verifies_access_control_on_chain_of_requires() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "id") {
                id: ID
                extra: String @external
                requiresExtra: String @requires(fields: "extra")
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type T @key(fields: "id") {
                id: ID
                secret: String @external
                extra: String @requires(fields: "secret")
              }
            "#,
        };

        let subgraph3 = ServiceDefinition {
            name: "Subgraph3",
            type_defs: r#"
              type T @key(fields: "id") {
                id: ID
                secret: String @authenticated @inaccessible
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2, subgraph3]);
        assert_composition_errors(
            &result,
            &[(
                "MISSING_TRANSITIVE_AUTH_REQUIREMENTS",
                r#"[Subgraph2] Field "T.extra" does not specify necessary @authenticated, @requiresScopes and/or @policy auth requirements to access the transitive field "T.secret" data from @requires selection set."#,
            )],
        );
    }

    #[test]
    fn works_with_chain_of_requires() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "id") {
                id: ID
                extra: String @external
                requiresExtra: String @requires(fields: "extra") @authenticated
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type T @key(fields: "id") {
                id: ID
                secret: String @external
                extra: String @requires(fields: "secret") @authenticated
              }
            "#,
        };

        let subgraph3 = ServiceDefinition {
            name: "Subgraph3",
            type_defs: r#"
              type T @key(fields: "id") {
                id: ID
                secret: String @authenticated @inaccessible
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2, subgraph3]);
        let _supergraph = result.expect("Expected composition to succeed");
    }

    #[test]
    fn requires_works_with_interface_object() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                i: I
              }

              type I @interfaceObject @key(fields: "id") {
                id: ID!
                extra: String @external
                requiresExtra: String @requires(fields: "extra") @authenticated
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              interface I @key(fields: "id") {
                id: ID!
                extra: String
              }

              type T @key(fields: "id") {
                id: ID
                extra: String @authenticated
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let supergraph = result.expect("Expected composition to succeed");
        let interface_i = supergraph
            .schema()
            .schema()
            .get_interface("I")
            .expect("interface I is defined");
        let requires_extra_field = interface_i
            .fields
            .get("requiresExtra")
            .expect("field requiresExtra exists");
        assert!(
            requires_extra_field
                .directives
                .iter()
                .any(|d| d.name == "authenticated")
        );
    }

    #[test]
    fn requires_works_with_interface_object_chains() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                i: I
              }

              type I @interfaceObject @key(fields: "id") {
                id: ID!
                extra: String @external
                requiresExtra: String @requires(fields: "extra") @authenticated
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type I @interfaceObject @key(fields: "id") {
                id: ID!
                secret: String @external
                extra: String @requires(fields: "secret") @authenticated
              }
            "#,
        };

        let subgraph3 = ServiceDefinition {
            name: "Subgraph3",
            type_defs: r#"
              interface I @key(fields: "id") {
                id: ID!
                secret: String
              }

              type T implements I @key(fields: "id") {
                id: ID!
                secret: String @authenticated
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2, subgraph3]);
        let _ = result.expect("Expected composition to succeed");
    }

    #[test]
    fn verifies_requires_on_interface_object_without_auth() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                i: I
              }

              type I @interfaceObject @key(fields: "id") {
                id: ID!
                extra: String @external
                requiresExtra: String @requires(fields: "extra")
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              interface I @key(fields: "id") {
                id: ID!
                extra: String
              }

              type T implements I @key(fields: "id") {
                id: ID!
                extra: String @authenticated
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        assert_composition_errors(
            &result,
            &[(
                "MISSING_TRANSITIVE_AUTH_REQUIREMENTS",
                r#"[Subgraph1] Field "I.requiresExtra" does not specify necessary @authenticated, @requiresScopes and/or @policy auth requirements to access the transitive field "I.extra" data from @requires selection set."#,
            )],
        );
    }

    #[test]
    fn requires_works_if_field_specifies_additional_auth() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "id") {
                id: ID
                extra: String @external
                requiresExtra: String @requires(fields: "extra") @requiresScopes(scopes: [["S1", "S2"]])
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type T @key(fields: "id") {
                id: ID
                extra: String @requiresScopes(scopes: [["S1"]])
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let _ = result.expect("Expected composition to succeed");
    }

    #[test]
    fn context_works_with_explicit_auth() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                t: T!
              }

              type T @key(fields: "id") @context(name: "context") {
                id: ID!
                u: U!
                prop: String! @authenticated
              }

              type U @key(fields: "id") {
                id: ID!
                field(a: String @fromContext(field: "$context { prop }")): Int! @authenticated
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type Query {
                a: Int!
              }

              type U @key(fields: "id") {
                id: ID!
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let _ = result.expect("Expected composition to succeed");
    }

    #[test]
    fn context_works_with_explicit_auth_and_multiple_contexts() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                foo: Foo!
                bar: Bar!
              }

              type Foo @key(fields: "id") @context(name: "context") {
                id: ID!
                u: U!
                prop: String! @requiresScopes(scopes: [["S1"]])
              }

              type Bar @key(fields: "id") @context(name: "context") {
                id: ID!
                u: U!
                prop: String! @requiresScopes(scopes: [["S2"]])
              }

              type U @key(fields: "id") {
                id: ID!
                field(a: String @fromContext(field: "$context { prop }")): Int! @requiresScopes(scopes: [["S1", "S2"]])
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type Query {
                a: Int!
              }

              type U @key(fields: "id") {
                id: ID!
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let _ = result.expect("Expected composition to succeed");
    }

    #[test]
    fn context_works_with_explicit_auth_and_multiple_contexts_using_type_conditions() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                foo: Foo!
                bar: Bar!
              }

              type Foo @key(fields: "id") @context(name: "context") {
                id: ID!
                u: U!
                prop: String! @requiresScopes(scopes: [["S1"]])
              }

              type Bar @key(fields: "id") @context(name: "context") {
                id: ID!
                u: U!
                prop2: String! @policy(policies: [["P1"]])
              }

              type U @key(fields: "id") {
                id: ID!
                field(
                  a: String
                  @fromContext(
                    field: "$context ... on Foo { prop } ... on Bar { prop2 }"
                  )
                ): Int! @requiresScopes(scopes: [["S1"]]) @policy(policies: [["P1"]])
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type Query {
                a: Int!
              }

              type U @key(fields: "id") {
                id: ID!
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let _ = result.expect("Expected composition to succeed");
    }

    #[test]
    fn context_does_not_work_with_missing_auth() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                t: T!
              }

              type T @key(fields: "id") @context(name: "context") {
                id: ID!
                u: U!
                prop: String! @authenticated
              }

              type U @key(fields: "id") {
                id: ID!
                field(a: String @fromContext(field: "$context { prop }")): Int!
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type Query {
                a: Int!
              }

              type U @key(fields: "id") {
                id: ID!
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        assert_composition_errors(
            &result,
            &[(
                "MISSING_TRANSITIVE_AUTH_REQUIREMENTS",
                r#"[Subgraph1] Field "U.field" does not specify necessary @authenticated, @requiresScopes and/or @policy auth requirements to access the transitive field "T.prop" data from @fromContext selection set."#,
            )],
        );
    }

    #[test]
    fn context_does_not_work_with_missing_auth_on_one_of_the_contexts() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                foo: Foo!
                bar: Bar!
              }

              type Foo @key(fields: "id") @context(name: "context") @authenticated {
                id: ID!
                u: U!
                prop: String!
              }

              type Bar @key(fields: "id") @context(name: "context") {
                id: ID!
                u: U!
                prop: String!
              }

              type U @key(fields: "id") {
                id: ID!
                field(a: String @fromContext(field: "$context { prop }")): Int!
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type Query {
                a: Int!
              }

              type U @key(fields: "id") {
                id: ID!
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        assert_composition_errors(
            &result,
            &[(
                "MISSING_TRANSITIVE_AUTH_REQUIREMENTS",
                r#"[Subgraph1] Field "U.field" does not specify necessary @authenticated, @requiresScopes and/or @policy auth requirements to access the transitive data in context Subgraph1__context from @fromContext selection set."#,
            )],
        );
    }
}

mod propagate_auth {
    use apollo_compiler::Name;
    use apollo_compiler::Node;
    use apollo_compiler::Schema;
    use apollo_compiler::ast::Value;
    use apollo_compiler::collections::IndexMap;
    use apollo_compiler::schema::Component;
    use apollo_compiler::schema::FieldDefinition;
    use apollo_federation::composition::compose;
    use apollo_federation::subgraph::typestate::Subgraph;

    use crate::composition::ServiceDefinition;
    use crate::composition::compose_as_fed2_subgraphs;

    #[test]
    fn propagates_authenticated_from_type_to_interface() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                i: I!
              }

              interface I {
                id: ID
              }

              type T implements I @key(fields: "id") @authenticated {
                id: ID
                value1: String
              }

              type U implements I @key(fields: "id") {
                id: ID
                value2: String
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "id") {
                id: ID!
                other: Int
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let supergraph = result.expect("Expected composition to succeed");
        let interface_i = supergraph
            .schema()
            .schema()
            .get_interface("I")
            .expect("interface I exists in schema");
        assert!(
            interface_i
                .directives
                .iter()
                .any(|d| d.name == "authenticated")
        );
    }

    #[test]
    fn propagates_requires_scopes_from_type_to_interface() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                i: I!
              }

              interface I {
                id: ID
              }

              type T implements I @key(fields: "id") @requiresScopes(scopes: [["S1"], ["S2"]]) {
                id: ID
                vT: String
              }

              type U implements I @key(fields: "id") @requiresScopes(scopes: [["S1"], ["S2", "S3"]]) {
                id: ID
                vU: String
              }

              type V implements I @key(fields: "id") {
                id: ID
                vV: String
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "id") {
                id: ID!
                other: Int
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let supergraph = result.expect("Expected composition to succeed");
        verify_type_access_control(
            supergraph.schema().schema(),
            "I",
            "requiresScopes",
            "scopes",
            vec![vec!["S1"], vec!["S2", "S3"]],
        );
    }

    #[test]
    fn propagates_policy_from_type_to_interface() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                i: I!
              }

              interface I {
                id: ID
              }

              type T implements I @key(fields: "id") @policy(policies: [["P1"]]) {
                id: ID
                vT: String
              }

              type U implements I @key(fields: "id") @policy(policies: [["P2"]]) {
                id: ID
                vU: String
              }

              type V implements I @key(fields: "id") {
                id: ID
                vV: String
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "id") {
                id: ID!
                other: Int
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let supergraph = result.expect("Expected composition to succeed");
        verify_type_access_control(
            supergraph.schema().schema(),
            "I",
            "policy",
            "policies",
            vec![vec!["P1", "P2"]],
        );
    }

    #[test]
    fn propagates_authenticated_from_object_field_to_interface_field() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                i: I!
              }

              interface I {
                id: ID
                i1: Int
                i2: String
                i3: String
              }

              type T1 implements I @key(fields: "id") {
                id: ID
                i1: Int
                i2: String @shareable
                i3: String
                value1: String
              }

              type T2 implements I @key(fields: "id") {
                id: ID
                i1: Int @authenticated
                i2: String
                i3: String
                value2: String
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type Query {
                t: T1
              }

              type T1 @key(fields: "id") {
                id: ID!
                i2: String @shareable @authenticated
                other: Int
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let supergraph = result.expect("Expected composition to succeed");
        let interface_i = supergraph
            .schema()
            .schema()
            .get_interface("I")
            .expect("interface I exists in schema");
        let field_i1 = interface_i.fields.get("i1").expect("field i1 exists");
        assert!(
            field_i1
                .directives
                .iter()
                .any(|d| d.name == "authenticated")
        );
        let field_i2 = interface_i.fields.get("i2").expect("field i2 exists");
        assert!(
            field_i2
                .directives
                .iter()
                .any(|d| d.name == "authenticated")
        );
    }

    #[test]
    fn propagates_requires_scopes_from_object_field_to_interface_field() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                i: I!
              }

              interface I {
                id: ID
                i1: Int
                i2: String
                i3: String
              }

              type T1 implements I @key(fields: "id") {
                id: ID
                i1: Int @requiresScopes(scopes: [["S1"]])
                i2: String @shareable
                i3: String
                value1: String
              }

              type T2 implements I @key(fields: "id") {
                id: ID
                i1: Int @requiresScopes(scopes: [["S1", "S2"]])
                i2: String
                i3: String
                value2: String
              }

              type T3 implements I @key(fields: "id") {
                id: ID
                i1: Int
                i2: String
                i3: String
                value2: String
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type Query {
                t: T1
              }

              type T1 @key(fields: "id") {
                id: ID!
                i2: String @shareable @requiresScopes(scopes: [["S3"]])
                other: Int
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let supergraph = result.expect("Expected composition to succeed");
        let interface_i = supergraph
            .schema()
            .schema()
            .get_interface("I")
            .expect("interface I exists in schema");
        verify_field_access_control(
            &interface_i.fields,
            "i1",
            "requiresScopes",
            "scopes",
            vec![vec!["S1", "S2"]],
        );
        verify_field_access_control(
            &interface_i.fields,
            "i2",
            "requiresScopes",
            "scopes",
            vec![vec!["S3"]],
        );
    }

    #[test]
    fn propagates_policy_from_object_field_to_interface_field() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                i: I!
              }

              interface I {
                id: ID
                i1: Int
                i2: String
                i3: String
              }

              type T1 implements I @key(fields: "id") {
                id: ID
                i1: Int @policy(policies: [["P1"], ["P2"]])
                i2: String @shareable
                i3: String
                value1: String
              }

              type T2 implements I @key(fields: "id") {
                id: ID
                i1: Int @policy(policies: [["P1"], ["P2", "P3"]])
                i2: String
                i3: String
                value2: String
              }

              type T3 implements I @key(fields: "id") {
                id: ID
                i1: Int
                i2: String
                i3: String
                value2: String
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type Query {
                t: T1
              }

              type T1 @key(fields: "id") {
                id: ID!
                i2: String @shareable @policy(policies: [["P4"]])
                other: Int
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let supergraph = result.expect("Expected composition to succeed");
        let interface_i = supergraph
            .schema()
            .schema()
            .get_interface("I")
            .expect("interface I exists in schema");
        verify_field_access_control(
            &interface_i.fields,
            "i1",
            "policy",
            "policies",
            vec![vec!["P1"], vec!["P2", "P3"]],
        );
        verify_field_access_control(
            &interface_i.fields,
            "i2",
            "policy",
            "policies",
            vec![vec!["P4"]],
        );
    }

    #[test]
    fn propagates_access_control_from_interface_object() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                i: I
              }

              type I @interfaceObject @key(fields: "id") {
                id: ID!
                secret: String @requiresScopes(scopes: [["S1"]])
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              interface I @key(fields: "id") {
                id: ID!
                extra: String
              }

              type T implements I @key(fields: "id") {
                id: ID!
                extra: String @authenticated
              }

              type U implements I @key(fields: "id") {
                id: ID!
                extra: String
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let supergraph = result.expect("Expected composition to succeed");
        let interface_i = supergraph
            .schema()
            .schema()
            .get_interface("I")
            .expect("interface I exists in schema");
        let field_extra = interface_i
            .fields
            .get("extra")
            .expect("field extra exists on interface I");
        assert!(
            field_extra
                .directives
                .iter()
                .any(|d| d.name == "authenticated")
        );

        let type_t = supergraph
            .schema()
            .schema()
            .get_object("T")
            .expect("type T exists in schema");
        verify_field_access_control(
            &type_t.fields,
            "secret",
            "requiresScopes",
            "scopes",
            vec![vec!["S1"]],
        );
    }

    #[test]
    fn works_with_shareable_interface_object_field() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                i: I
              }

              type I @interfaceObject @key(fields: "id") {
                id: ID!
                secret: String @requiresScopes(scopes: [["S1"]]) @shareable
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              interface I @key(fields: "id") {
                id: ID!
                extra: String
              }

              type T implements I @key(fields: "id") {
                id: ID!
                extra: String @authenticated
              }

              type U implements I @key(fields: "id") {
                id: ID!
                extra: String
              }
            "#,
        };

        let subgraph3 = ServiceDefinition {
            name: "Subgraph3",
            type_defs: r#"
              type T @key(fields: "id") {
                id: ID!
                secret: String @requiresScopes(scopes: [["S2"]]) @shareable
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2, subgraph3]);
        let supergraph = result.expect("Expected composition to succeed");
        // interface I {
        //   id: ID!
        //   extra: String @authenticated
        //   secret: String @requiresScopes(scopes: [["S1", "S2"]])
        // }
        let interface_i = supergraph
            .schema()
            .schema()
            .get_interface("I")
            .expect("interface I exists in schema");
        let field_extra = interface_i
            .fields
            .get("extra")
            .expect("field extra exists on interface I");
        assert!(
            field_extra
                .directives
                .iter()
                .any(|d| d.name == "authenticated")
        );
        verify_field_access_control(
            &interface_i.fields,
            "secret",
            "requiresScopes",
            "scopes",
            vec![vec!["S1", "S2"]],
        );

        // type T implements I {
        //   id: ID!
        //   extra: String @authenticated
        //   secret: String @requiresScopes(scopes: [["S1", "S2"]])
        // }
        let type_t = supergraph
            .schema()
            .schema()
            .get_object("T")
            .expect("type T exists in schema");
        let field_extra = type_t
            .fields
            .get("extra")
            .expect("field extra exists on type T");
        assert!(
            field_extra
                .directives
                .iter()
                .any(|d| d.name == "authenticated")
        );
        verify_field_access_control(
            &type_t.fields,
            "secret",
            "requiresScopes",
            "scopes",
            vec![vec!["S1", "S2"]],
        );

        // type U implements I {
        //   id: ID!
        //   extra: String
        //   secret: String @requiresScopes(scopes: [["S1"]])
        // }
        let type_u = supergraph
            .schema()
            .schema()
            .get_object("U")
            .expect("type U exists in schema");
        let field_extra = type_u
            .fields
            .get("extra")
            .expect("field extra exists on type U");
        assert!(
            !field_extra
                .directives
                .iter()
                .any(|d| d.name == "authenticated")
        );
        verify_field_access_control(
            &type_u.fields,
            "secret",
            "requiresScopes",
            "scopes",
            vec![vec!["S1"]],
        );
    }

    #[test]
    fn propagates_type_access_control_on_interface_chains() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                node(id: ID!): Node
              }

              interface Node {
                id: ID!
              }

              interface I implements Node {
                id: ID!
                intf: String
              }

              type T implements Node & I @key(fields: "id") @policy(policies: [["P1"]]) {
                id: ID!
                intf: String
                vT: String
              }

              type U implements Node & I @key(fields: "id") @policy(policies: [["P2"]]) {
                id: ID!
                intf: String
                vU: String
              }

              type V implements Node & I @key(fields: "id") {
                id: ID!
                intf: String
                vV: String
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              type Query {
                t: T
              }

              type T @key(fields: "id") {
                id: ID!
                other: Int
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let supergraph = result.expect("Expected composition to succeed");
        verify_type_access_control(
            supergraph.schema().schema(),
            "Node",
            "policy",
            "policies",
            vec![vec!["P1", "P2"]],
        );
        verify_type_access_control(
            supergraph.schema().schema(),
            "I",
            "policy",
            "policies",
            vec![vec!["P1", "P2"]],
        );
    }

    #[test]
    fn propagates_field_access_control_on_interface_chains_with_interface_object() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              type Query {
                i: I
              }

              type I @interfaceObject @key(fields: "id") {
                id: ID!
                secret: String @requiresScopes(scopes: [["S1"]])
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              interface Node {
                id: ID!
                secret: String
              }

              interface I implements Node @key(fields: "id") {
                id: ID!
                extra: String
                secret: String
              }

              type T implements Node & I @key(fields: "id") {
                id: ID!
                extra: String @authenticated
                secret: String @external
              }

              type U implements Node & I @key(fields: "id") {
                id: ID!
                extra: String
                secret: String @external
              }
            "#,
        };

        let result = compose_as_fed2_subgraphs(&[subgraph1, subgraph2]);
        let supergraph = result.expect("Expected composition to succeed");
        println!("{}", supergraph.schema().schema());

        let interface_node = supergraph
            .schema()
            .schema()
            .get_interface("Node")
            .expect("interface Node exists in the schema");
        verify_field_access_control(
            &interface_node.fields,
            "secret",
            "requiresScopes",
            "scopes",
            vec![vec!["S1"]],
        );

        let interface_i = supergraph
            .schema()
            .schema()
            .get_interface("I")
            .expect("interface I exists in the schema");
        verify_field_access_control(
            &interface_i.fields,
            "secret",
            "requiresScopes",
            "scopes",
            vec![vec!["S1"]],
        );
        let field_extra_i = interface_i
            .fields
            .get("extra")
            .expect("field extra exists on interface I");
        assert!(
            field_extra_i
                .directives
                .iter()
                .any(|d| d.name == "authenticated")
        );
    }

    #[test]
    fn access_control_propagation_handles_renames() {
        let subgraph1 = ServiceDefinition {
            name: "Subgraph1",
            type_defs: r#"
              extend schema @link(
                url: "https://specs.apollo.dev/federation/v2.9",
                import: [ "@key", { name: "@policy", as: "@apolloPolicy" }]
              )

              type Query {
                i: I!
              }

              interface I {
                id: ID
              }

              type T implements I @key(fields: "id") @apolloPolicy(policies: [["P1"]]) {
                id: ID
                vT: String
              }

              type U implements I @key(fields: "id") @apolloPolicy(policies: [["P2"]]) {
                id: ID
                vU: String
              }

              type V implements I @key(fields: "id") {
                id: ID
                vV: String
              }
            "#,
        };

        let subgraph2 = ServiceDefinition {
            name: "Subgraph2",
            type_defs: r#"
              extend schema @link(
                url: "https://specs.apollo.dev/federation/v2.9",
                import: [ "@key", { name: "@authenticated", as: "@apolloAuthenticated" }]
              )

              type Query {
                t: T
              }

              type T @key(fields: "id") {
                id: ID!
                other: Int @apolloAuthenticated
              }
            "#,
        };

        let subgraphs = vec![
            Subgraph::parse(
                subgraph1.name,
                &format!("http://{}", subgraph1.name),
                subgraph1.type_defs,
            )
            .expect("valid subgraph"),
            Subgraph::parse(
                subgraph2.name,
                &format!("http://{}", subgraph2.name),
                subgraph2.type_defs,
            )
            .expect("valid subgraph"),
        ];
        let result = compose(subgraphs);
        let supergraph = result.expect("Expected composition to succeed");

        verify_type_access_control(
            supergraph.schema().schema(),
            "I",
            "apolloPolicy",
            "policies",
            vec![vec!["P1", "P2"]],
        );
        let type_t = supergraph
            .schema()
            .schema()
            .get_object("T")
            .expect("type T exists in the schema");
        let field_other_t = type_t
            .fields
            .get("other")
            .expect("field other exists on type T");
        assert!(
            field_other_t
                .directives
                .iter()
                .any(|d| d.name == "apolloAuthenticated")
        );
    }

    // this is a copy of a test util method from argument_composition_strategies.rs
    // we need this duplicated as
    // - this should not be a public API
    // - the arg composition strategy tests should be unit tests
    fn parse_into_ast_value_list(value: Vec<Vec<&str>>) -> Value {
        // outer array is interpreted as the disjunction (an OR) of the inner arrays.
        let mut disjunctions = vec![];
        for inner_array in value {
            // inner array is interpreted as the conjunction (an AND) of the conditions in the array.
            let mut conjunctions = vec![];
            for value in inner_array {
                conjunctions.push(Node::new(Value::String(value.to_string())));
            }
            disjunctions.push(Node::new(Value::List(conjunctions)));
        }
        Value::List(disjunctions)
    }

    fn verify_type_access_control(
        schema: &Schema,
        target_type: &str,
        target_directive: &str,
        arg_name: &str,
        expected: Vec<Vec<&str>>,
    ) {
        let extended_type = schema
            .types
            .get(target_type)
            .unwrap_or_else(|| panic!("{target_type} exists in schema"));
        let directive = extended_type
            .directives()
            .iter()
            .find(|d| d.name == target_directive)
            .unwrap_or_else(|| panic!("@{target_directive} is applied on target {target_type}"));
        let arg = directive
            .specified_argument_by_name(arg_name)
            .unwrap_or_else(|| {
                panic!("{arg_name} argument should exist on directive {target_directive}")
            });
        let expected = parse_into_ast_value_list(expected);
        assert_eq!(arg.as_ref(), &expected);
    }

    fn verify_field_access_control(
        fields: &IndexMap<Name, Component<FieldDefinition>>,
        field_name: &str,
        target_directive: &str,
        arg_name: &str,
        expected: Vec<Vec<&str>>,
    ) {
        let field = fields
            .get(field_name)
            .unwrap_or_else(|| panic!("field {field_name} exists"));
        let directive = field
            .directives
            .iter()
            .find(|d| d.name == target_directive)
            .unwrap_or_else(|| {
                panic!("@{target_directive} is applied on target {field_name} field")
            });
        let arg = directive
            .specified_argument_by_name(arg_name)
            .unwrap_or_else(|| {
                panic!("{arg_name} argument should exist on directive {target_directive}")
            });
        let expected = parse_into_ast_value_list(expected);
        assert_eq!(arg.as_ref(), &expected);
    }
}
