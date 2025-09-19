use super::{ServiceDefinition, assert_composition_errors, compose_as_fed2_subgraphs};
use apollo_compiler::schema::ExtendedType;
use apollo_federation::composition::Satisfiable;
use apollo_federation::supergraph::Supergraph;

/// Validates that @inaccessible directives are properly propagated to the supergraph schema
fn validate_inaccessible_propagation(supergraph: &Supergraph<Satisfiable>) {
    let schema = supergraph.schema().schema();

    // Check for @inaccessible directive on Query.me field
    if let Some(query_type_name) = &schema.schema_definition.query {
        if let Some(ExtendedType::Object(query_obj)) = schema.types.get(query_type_name.as_str()) {
            if let Some(me_field) = query_obj.fields.get("me") {
                let inaccessible_directives: Vec<_> = me_field
                    .directives
                    .iter()
                    .filter(|d| d.name == "inaccessible")
                    .collect();
                assert!(
                    !inaccessible_directives.is_empty(),
                    "Expected @inaccessible directive on Query.me field"
                );
            }
        }
    }

    // Check for @inaccessible directive on User.age field
    if let Some(ExtendedType::Object(user_type)) = schema.types.get("User") {
        if let Some(age_field) = user_type.fields.get("age") {
            let inaccessible_directives: Vec<_> = age_field
                .directives
                .iter()
                .filter(|d| d.name == "inaccessible")
                .collect();
            assert!(
                !inaccessible_directives.is_empty(),
                "Expected @inaccessible directive on User.age field"
            );
        }
    }
}

/// Validates that @inaccessible directives are properly merged on the same element
fn validate_inaccessible_merging(supergraph: &Supergraph<Satisfiable>) {
    let schema = supergraph.schema().schema();

    // Check that @inaccessible directive is present on User.name field (merged from both subgraphs)
    if let Some(ExtendedType::Object(user_type)) = schema.types.get("User") {
        if let Some(name_field) = user_type.fields.get("name") {
            let inaccessible_directives: Vec<_> = name_field
                .directives
                .iter()
                .filter(|d| d.name == "inaccessible")
                .collect();
            assert!(
                !inaccessible_directives.is_empty(),
                "Expected @inaccessible directive on User.name field"
            );
        }
    }
}

// =============================================================================
// @inaccessible DIRECTIVE PROPAGATION - Tests for @inaccessible propagation
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
fn inaccessible_propagates_to_supergraph() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          me: User @inaccessible
          users: [User]
        }

        type User @key(fields: "id") {
          id: ID!
          name: String!
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User @key(fields: "id") {
          id: ID!
          birthdate: String!
          age: Int! @inaccessible
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");

    // Validate that @inaccessible directives are properly propagated to the supergraph schema
    validate_inaccessible_propagation(&supergraph);
}

#[test]
#[ignore = "until merge implementation completed"]
fn inaccessible_merges_on_same_element() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          user: [User]
        }

        type User @key(fields: "id") {
          id: ID!
          name: String @shareable @inaccessible
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User @key(fields: "id") {
          id: ID!
          name: String @shareable @inaccessible
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");

    // Validate that @inaccessible directives are properly merged
    validate_inaccessible_merging(&supergraph);
}

#[test]
#[ignore = "until merge implementation completed"]
fn inaccessible_rejects_inaccessible_and_external_together() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          user: [User]
        }

        type User @key(fields: "id") {
          id: ID!
          name: String!
          birthdate: Int! @external @inaccessible
          age: Int! @requires(fields: "birthdate")
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User @key(fields: "id") {
          id: ID!
          birthdate: Int!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "MERGED_DIRECTIVE_APPLICATION_ON_EXTERNAL",
            r#"[subgraphA] Cannot apply merged directive @inaccessible to external field "User.birthdate""#,
        )],
    );
}

#[test]
#[ignore = "until merge implementation completed"]
fn inaccessible_errors_if_imported_under_mismatched_names() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@inaccessible", as: "@private"}])

        type Query {
          q1: Int @private
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@inaccessible"])

        type Query {
          q2: Int @inaccessible
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "LINK_IMPORT_NAME_MISMATCH",
            r#"The "@inaccessible" directive (from https://specs.apollo.dev/federation/v2.0) is imported with mismatched name between subgraphs: it is imported as "@inaccessible" in subgraph "subgraphB" but "@private" in subgraph "subgraphA""#,
        )],
    );
}

#[test]
#[ignore = "until merge implementation completed"]
fn inaccessible_succeeds_if_imported_under_same_non_default_name() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@inaccessible", as: "@private"}])

        type Query {
          q1: Int @private
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@inaccessible", as: "@private"}])

        type Query {
          q2: Int @private
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let _supergraph =
        result.expect("Expected composition to succeed with consistent @inaccessible import names");
}

#[test]
#[ignore = "until merge implementation completed"]
fn inaccessible_ignores_inaccessible_element_when_validating_composition() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          user: User
        }

        type User @key(fields: "id") {
          id: ID!
          name: String! @inaccessible
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User @key(fields: "id") {
          id: ID!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let _supergraph = result.expect("Expected composition to succeed - @inaccessible fields should be ignored during validation");
}

#[test]
#[ignore = "until merge implementation completed"]
fn inaccessible_errors_if_subgraph_misuses_inaccessible() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          user: User
        }

        type User @key(fields: "id") {
          id: ID!
          name: String! @inaccessible
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User @key(fields: "id") {
          id: ID!
          name: String!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "INVALID_INACCESSIBLE_USAGE",
            r#"Field "User.name" is marked @inaccessible in subgraph "subgraphA" but not in subgraph "subgraphB""#,
        )],
    );
}

#[test]
#[ignore = "until merge implementation completed"]
fn inaccessible_uses_security_core_purpose_in_supergraph() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          me: User @inaccessible
        }

        type User @key(fields: "id") {
          id: ID!
          name: String!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a]);
    let supergraph = result.expect("Expected composition to succeed");
    // Assert there is a @link with `for: SECURITY` on the supergraph schema
    let schema = supergraph.schema().schema();
    let has_security_purpose_link = schema.schema_definition.directives.iter().any(|d| {
        d.name == "link"
            && matches!(
                d.argument_by_name("for", schema).map(|a| a.as_ref()),
                Ok(apollo_compiler::ast::Value::Enum(enum_name)) if enum_name == "SECURITY"
            )
    });
    assert!(
        has_security_purpose_link,
        "Expected a @link with for: SECURITY in the supergraph schema",
    );
}
