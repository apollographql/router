// Ported from federation/composition-js/src/__tests__/compose.test.ts
// Original describe block: '@authenticated'

use super::ServiceDefinition;
use super::assert_api_schema_snapshot;
use super::assert_composition_success;
use super::compose_as_fed2_subgraphs;
use super::error_messages;

#[ignore = "until merge implementation completed"]
#[test]
fn comprehensive_locations() {
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

    let on_interface = ServiceDefinition {
        name: "on-interface",
        type_defs: r#"
        type Query {
            interface: AuthenticatedInterface!
        }

        interface AuthenticatedInterface @authenticated {
            field: Int!
        }
        "#,
    };

    let on_interface_object = ServiceDefinition {
        name: "on-interface-object",
        type_defs: r#"
        type AuthenticatedInterfaceObject
            @interfaceObject
            @key(fields: "id")
            @authenticated
        {
            id: String!
        }
        "#,
    };

    let on_scalar = ServiceDefinition {
        name: "on-scalar",
        type_defs: r#"
        scalar AuthenticatedScalar @authenticated

        # This needs to exist in at least one other subgraph from where it's defined
        # as an @interfaceObject (so arbitrarily adding it here). We don't actually
        # apply @authenticated to this one since we want to see it propagate even
        # when it's not applied in all locations.
        interface AuthenticatedInterfaceObject @key(fields: "id") {
            id: String!
        }
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
        on_interface,
        on_interface_object,
        on_scalar,
        on_enum,
        on_root_field,
        on_object_field,
        on_entity_field,
    ]);
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
#[test]
fn authenticated_has_correct_definition_in_supergraph() {
    let a = ServiceDefinition {
        name: "a",
        type_defs: r#"
        type Query {
            x: Int @authenticated
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[a]);
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}

#[ignore = "until merge implementation completed"]
#[test]
fn applies_authenticated_on_types_as_long_as_it_is_used_once() {
    let a1 = ServiceDefinition {
        name: "a1",
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

    let a2 = ServiceDefinition {
        name: "a2",
        type_defs: r#"
        type A @key(fields: "id") {
            id: String!
            a2: String
        }
        "#,
    };

    // checking composition in either order (not sure if this is necessary but
    // it's not hurting anything)
    let result1 = compose_as_fed2_subgraphs(&[a1, a2]);
    let supergraph1 = assert_composition_success(&result1);

    assert_api_schema_snapshot(supergraph1);
}

#[ignore = "until merge implementation completed"]
#[test]
fn validation_error_on_incompatible_directive_definition() {
    let invalid_definition = ServiceDefinition {
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

    let result = compose_as_fed2_subgraphs(&[invalid_definition]);
    assert!(result.is_err());

    let errors = error_messages(&result);
    assert_eq!(
        errors,
        vec![
            "[invalidDefinition] Invalid definition for directive \"@authenticated\": \"@authenticated\" should have locations FIELD_DEFINITION, OBJECT, INTERFACE, SCALAR, ENUM, but found (non-subset) ENUM_VALUE"
        ]
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn validation_error_on_invalid_application() {
    let invalid_application = ServiceDefinition {
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

    let result = compose_as_fed2_subgraphs(&[invalid_application]);
    assert!(result.is_err());

    let errors = error_messages(&result);
    assert!(errors.iter().any(|error| {
        error.contains("authenticated directive is not supported for ENUM_VALUE location")
    }));
}

#[ignore = "until merge implementation completed"]
#[test]
fn existing_authenticated_directive_with_fed_1() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        directive @authenticated(scope: [String!]) repeatable on FIELD_DEFINITION

        extend type Foo @key(fields: "id") {
          id: ID!
          protected: String @authenticated(scope: ["foo"])
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Query {
          foo: Foo
        }

        type Foo @key(fields: "id") {
          id: ID!
          name: String!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = assert_composition_success(&result);

    // Verify that the custom @authenticated directive is not in the final schema
    // (it should be filtered out since it's not the federation @authenticated directive)
    assert_api_schema_snapshot(supergraph);
}
