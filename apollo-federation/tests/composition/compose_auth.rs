use apollo_compiler::coord;
use apollo_compiler::schema::ExtendedType;
use insta::assert_snapshot;
use test_log::test;

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
    let supergraph = result.expect("Expected composition to succeed");

    let schema = supergraph.schema().schema();

    // Validate @authenticated is applied to all expected elements:
    // ["AuthenticatedObject", "AuthenticatedInterface", "AuthenticatedInterfaceObject",
    //  "AuthenticatedScalar", "AuthenticatedEnum", "Query.authenticatedRootField",
    //  "ObjectWithAuthenticatedField.field", "EntityWithAuthenticatedField.field"]

    for coord in [
        coord!(AuthenticatedObject),
        coord!(AuthenticatedInterface),
        coord!(AuthenticatedInterfaceObject),
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
#[ignore = "until merge implementation completed"]
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

    let on_interface = ServiceDefinition {
        name: "on-interface",
        type_defs: r#"
        type Query {
          interface: ScopedInterface!
        }

        interface ScopedInterface @requiresScopes(scopes: ["interface"]) {
          field: Int!
        }
        "#,
    };

    let on_interface_object = ServiceDefinition {
        name: "on-interface-object",
        type_defs: r#"
        type ScopedInterfaceObject
          @interfaceObject
          @key(fields: "id")
          @requiresScopes(scopes: ["interfaceObject"])
        {
          id: String!
        }
        "#,
    };

    let on_scalar = ServiceDefinition {
        name: "on-scalar",
        type_defs: r#"
        scalar ScopedScalar @requiresScopes(scopes: ["scalar"])

        # This needs to exist in at least one other subgraph from where it's defined
        # as an @interfaceObject (so arbitrarily adding it here). We don't actually
        # apply @requiresScopes to this one since we want to see it propagate even
        # when it's not applied in all locations.
        interface ScopedInterfaceObject @key(fields: "id") {
          id: String!
        }
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
        on_interface,
        on_interface_object,
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
    // ["ScopedObject", "ScopedInterface", "ScopedInterfaceObject",
    //  "ScopedScalar", "ScopedEnum", "Query.scopedRootField",
    //  "ObjectWithScopedField.field", "EntityWithScopedField.field"]

    // ScopedObject
    let scoped_object = schema
        .types
        .get("ScopedObject")
        .expect("ScopedObject exists");
    if let ExtendedType::Object(object) = scoped_object {
        assert!(object.directives.iter().any(|d| d.name == "requiresScopes"));
    } else {
        panic!("ScopedObject is not an object");
    }

    // ScopedInterface
    let scoped_interface = schema
        .types
        .get("ScopedInterface")
        .expect("ScopedInterface exists");
    if let ExtendedType::Interface(interface) = scoped_interface {
        assert!(
            interface
                .directives
                .iter()
                .any(|d| d.name == "requiresScopes")
        );
    } else {
        panic!("ScopedInterface is not an interface");
    }

    // ScopedInterfaceObject
    let scoped_interface_object = schema
        .types
        .get("ScopedInterfaceObject")
        .expect("ScopedInterfaceObject exists");
    if let ExtendedType::Object(object) = scoped_interface_object {
        assert!(object.directives.iter().any(|d| d.name == "requiresScopes"));
    } else {
        panic!("ScopedInterfaceObject is not an object");
    }

    // ScopedScalar
    let scoped_scalar = schema
        .types
        .get("ScopedScalar")
        .expect("ScopedScalar exists");
    if let ExtendedType::Scalar(scalar) = scoped_scalar {
        assert!(scalar.directives.iter().any(|d| d.name == "requiresScopes"));
    } else {
        panic!("ScopedScalar is not a scalar");
    }

    // ScopedEnum
    let scoped_enum = schema.types.get("ScopedEnum").expect("ScopedEnum exists");
    if let ExtendedType::Enum(enum_type) = scoped_enum {
        assert!(
            enum_type
                .directives
                .iter()
                .any(|d| d.name == "requiresScopes")
        );
    } else {
        panic!("ScopedEnum is not an enum");
    }

    // Query.scopedRootField
    if let Some(query_type_name) = &schema.schema_definition.query
        && let Some(ExtendedType::Object(query_obj)) = schema.types.get(query_type_name.as_str())
    {
        if let Some(scoped_root_field) = query_obj.fields.get("scopedRootField") {
            assert!(
                scoped_root_field
                    .directives
                    .iter()
                    .any(|d| d.name == "requiresScopes")
            );
        } else {
            panic!("scopedRootField not found on Query");
        }
    }

    // ObjectWithScopedField.field
    let object_with_field = schema
        .types
        .get("ObjectWithScopedField")
        .expect("ObjectWithScopedField exists");
    if let ExtendedType::Object(object) = object_with_field {
        if let Some(field) = object.fields.get("field") {
            assert!(field.directives.iter().any(|d| d.name == "requiresScopes"));
        } else {
            panic!("field not found on ObjectWithScopedField");
        }
    } else {
        panic!("ObjectWithScopedField is not an object");
    }

    // EntityWithScopedField.field
    let entity_with_field = schema
        .types
        .get("EntityWithScopedField")
        .expect("EntityWithScopedField exists");
    if let ExtendedType::Object(object) = entity_with_field {
        if let Some(field) = object.fields.get("field") {
            assert!(field.directives.iter().any(|d| d.name == "requiresScopes"));
        } else {
            panic!("field not found on EntityWithScopedField");
        }
    } else {
        panic!("EntityWithScopedField is not an object");
    }
}

// =============================================================================
// @policy DIRECTIVE TESTS - Tests for @policy functionality
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
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

    let on_interface = ServiceDefinition {
        name: "on-interface",
        type_defs: r#"
        type Query {
          interface: ScopedInterface!
        }

        interface ScopedInterface @policy(policies: ["interface"]) {
          field: Int!
        }
        "#,
    };

    let on_interface_object = ServiceDefinition {
        name: "on-interface-object",
        type_defs: r#"
        type ScopedInterfaceObject
          @interfaceObject
          @key(fields: "id")
          @policy(policies: ["interfaceObject"])
        {
          id: String!
        }
        "#,
    };

    let on_scalar = ServiceDefinition {
        name: "on-scalar",
        type_defs: r#"
        scalar ScopedScalar @policy(policies: ["scalar"])

        # This needs to exist in at least one other subgraph from where it's defined
        # as an @interfaceObject (so arbitrarily adding it here). We don't actually
        # apply @policy to this one since we want to see it propagate even
        # when it's not applied in all locations.
        interface ScopedInterfaceObject @key(fields: "id") {
          id: String!
        }
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
        on_interface,
        on_interface_object,
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
    // ["ScopedObject", "ScopedInterface", "ScopedInterfaceObject",
    //  "ScopedScalar", "ScopedEnum", "Query.scopedRootField",
    //  "ObjectWithScopedField.field", "EntityWithScopedField.field"]

    // ScopedObject
    let scoped_object = schema
        .types
        .get("ScopedObject")
        .expect("ScopedObject exists");
    if let ExtendedType::Object(object) = scoped_object {
        assert!(object.directives.iter().any(|d| d.name == "policy"));
    } else {
        panic!("ScopedObject is not an object");
    }

    // ScopedInterface
    let scoped_interface = schema
        .types
        .get("ScopedInterface")
        .expect("ScopedInterface exists");
    if let ExtendedType::Interface(interface) = scoped_interface {
        assert!(interface.directives.iter().any(|d| d.name == "policy"));
    } else {
        panic!("ScopedInterface is not an interface");
    }

    // ScopedInterfaceObject
    let scoped_interface_object = schema
        .types
        .get("ScopedInterfaceObject")
        .expect("ScopedInterfaceObject exists");
    if let ExtendedType::Object(object) = scoped_interface_object {
        assert!(object.directives.iter().any(|d| d.name == "policy"));
    } else {
        panic!("ScopedInterfaceObject is not an object");
    }

    // ScopedScalar
    let scoped_scalar = schema
        .types
        .get("ScopedScalar")
        .expect("ScopedScalar exists");
    if let ExtendedType::Scalar(scalar) = scoped_scalar {
        assert!(scalar.directives.iter().any(|d| d.name == "policy"));
    } else {
        panic!("ScopedScalar is not a scalar");
    }

    // ScopedEnum
    let scoped_enum = schema.types.get("ScopedEnum").expect("ScopedEnum exists");
    if let ExtendedType::Enum(enum_type) = scoped_enum {
        assert!(enum_type.directives.iter().any(|d| d.name == "policy"));
    } else {
        panic!("ScopedEnum is not an enum");
    }

    // Query.scopedRootField
    if let Some(query_type_name) = &schema.schema_definition.query
        && let Some(ExtendedType::Object(query_obj)) = schema.types.get(query_type_name.as_str())
    {
        if let Some(scoped_root_field) = query_obj.fields.get("scopedRootField") {
            assert!(
                scoped_root_field
                    .directives
                    .iter()
                    .any(|d| d.name == "policy")
            );
        } else {
            panic!("scopedRootField not found on Query");
        }
    }

    // ObjectWithScopedField.field
    let object_with_field = schema
        .types
        .get("ObjectWithScopedField")
        .expect("ObjectWithScopedField exists");
    if let ExtendedType::Object(object) = object_with_field {
        if let Some(field) = object.fields.get("field") {
            assert!(field.directives.iter().any(|d| d.name == "policy"));
        } else {
            panic!("field not found on ObjectWithScopedField");
        }
    } else {
        panic!("ObjectWithScopedField is not an object");
    }

    // EntityWithScopedField.field
    let entity_with_field = schema
        .types
        .get("EntityWithScopedField")
        .expect("EntityWithScopedField exists");
    if let ExtendedType::Object(object) = entity_with_field {
        if let Some(field) = object.fields.get("field") {
            assert!(field.directives.iter().any(|d| d.name == "policy"));
        } else {
            panic!("field not found on EntityWithScopedField");
        }
    } else {
        panic!("EntityWithScopedField is not an object");
    }
}
