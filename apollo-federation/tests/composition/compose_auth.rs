use apollo_compiler::schema::ExtendedType;
use insta::assert_snapshot;

use super::{ServiceDefinition, assert_composition_errors, compose_as_fed2_subgraphs};

/// Helper function to print directive definition for snapshot comparison
fn print_directive_definition(directive: &apollo_compiler::schema::DirectiveDefinition) -> String {
    directive.to_string()
}

// =============================================================================
// @authenticated DIRECTIVE TESTS - Tests for @authenticated functionality
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
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

    // AuthenticatedObject
    let authenticated_object = schema
        .types
        .get("AuthenticatedObject")
        .expect("AuthenticatedObject exists");
    if let ExtendedType::Object(object) = authenticated_object {
        assert!(object.directives.iter().any(|d| d.name == "authenticated"));
    } else {
        panic!("AuthenticatedObject is not an object");
    }

    // AuthenticatedInterface
    let authenticated_interface = schema
        .types
        .get("AuthenticatedInterface")
        .expect("AuthenticatedInterface exists");
    if let ExtendedType::Interface(interface) = authenticated_interface {
        assert!(
            interface
                .directives
                .iter()
                .any(|d| d.name == "authenticated")
        );
    } else {
        panic!("AuthenticatedInterface is not an interface");
    }

    // AuthenticatedInterfaceObject
    let authenticated_interface_object = schema
        .types
        .get("AuthenticatedInterfaceObject")
        .expect("AuthenticatedInterfaceObject exists");
    if let ExtendedType::Object(object) = authenticated_interface_object {
        assert!(object.directives.iter().any(|d| d.name == "authenticated"));
    } else {
        panic!("AuthenticatedInterfaceObject is not an object");
    }

    // AuthenticatedScalar
    let authenticated_scalar = schema
        .types
        .get("AuthenticatedScalar")
        .expect("AuthenticatedScalar exists");
    if let ExtendedType::Scalar(scalar) = authenticated_scalar {
        assert!(scalar.directives.iter().any(|d| d.name == "authenticated"));
    } else {
        panic!("AuthenticatedScalar is not a scalar");
    }

    // AuthenticatedEnum
    let authenticated_enum = schema
        .types
        .get("AuthenticatedEnum")
        .expect("AuthenticatedEnum exists");
    if let ExtendedType::Enum(enum_type) = authenticated_enum {
        assert!(
            enum_type
                .directives
                .iter()
                .any(|d| d.name == "authenticated")
        );
    } else {
        panic!("AuthenticatedEnum is not an enum");
    }

    // Query.authenticatedRootField
    if let Some(query_type_name) = &schema.schema_definition.query {
        if let Some(ExtendedType::Object(query_obj)) = schema.types.get(query_type_name.as_str()) {
            if let Some(authenticated_root_field) = query_obj.fields.get("authenticatedRootField") {
                assert!(
                    authenticated_root_field
                        .directives
                        .iter()
                        .any(|d| d.name == "authenticated")
                );
            } else {
                panic!("authenticatedRootField not found on Query");
            }
        }
    }

    // ObjectWithAuthenticatedField.field
    let object_with_field = schema
        .types
        .get("ObjectWithAuthenticatedField")
        .expect("ObjectWithAuthenticatedField exists");
    if let ExtendedType::Object(object) = object_with_field {
        if let Some(field) = object.fields.get("field") {
            assert!(field.directives.iter().any(|d| d.name == "authenticated"));
        } else {
            panic!("field not found on ObjectWithAuthenticatedField");
        }
    } else {
        panic!("ObjectWithAuthenticatedField is not an object");
    }

    // EntityWithAuthenticatedField.field
    let entity_with_field = schema
        .types
        .get("EntityWithAuthenticatedField")
        .expect("EntityWithAuthenticatedField exists");
    if let ExtendedType::Object(object) = entity_with_field {
        if let Some(field) = object.fields.get("field") {
            assert!(field.directives.iter().any(|d| d.name == "authenticated"));
        } else {
            panic!("field not found on EntityWithAuthenticatedField");
        }
    } else {
        panic!("EntityWithAuthenticatedField is not an object");
    }
}

#[test]
#[ignore = "until merge implementation completed"]
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
    assert_snapshot!(print_directive_definition(authenticated_directive), @"directive @authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM");
}

#[test]
#[ignore = "until merge implementation completed"]
fn authenticated_applies_on_types_as_long_as_used_once() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          t: T
        }

        type T @authenticated {
          a: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type T {
          b: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");
    let schema = supergraph.schema().schema();

    // Validate that @authenticated is applied to type T in the supergraph
    // even though it's only present in subgraphA
    let type_t = schema
        .types
        .get("T")
        .expect("Type T should exist in supergraph");
    if let ExtendedType::Object(object) = type_t {
        assert!(
            object.directives.iter().any(|d| d.name == "authenticated"),
            "Expected @authenticated directive on type T in supergraph"
        );
    } else {
        panic!("Type T should be an object type");
    }
}

#[test]
#[ignore = "until merge implementation completed"]
fn authenticated_validation_error_on_incompatible_directive_definition() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          x: Int @authenticated
        }

        directive @authenticated on OBJECT
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a]);
    assert_composition_errors(
        &result,
        &[(
            "DIRECTIVE_DEFINITION_INVALID",
            r#"Directive definition for "@authenticated" is incompatible with federation specification"#,
        )],
    );
}

#[test]
#[ignore = "until merge implementation completed"]
fn authenticated_validation_error_on_invalid_application() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        input MyInput @authenticated {
          field: String
        }

        type Query {
          query(input: MyInput): String
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a]);
    assert_composition_errors(
        &result,
        &[(
            "DIRECTIVE_INVALID_USAGE",
            r#"Directive "@authenticated" cannot be applied to input types"#,
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
    if let Some(query_type_name) = &schema.schema_definition.query {
        if let Some(ExtendedType::Object(query_obj)) = schema.types.get(query_type_name.as_str()) {
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
    if let Some(query_type_name) = &schema.schema_definition.query {
        if let Some(ExtendedType::Object(query_obj)) = schema.types.get(query_type_name.as_str()) {
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
