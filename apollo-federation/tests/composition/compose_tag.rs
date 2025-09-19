use apollo_compiler::schema::ExtendedType;
use apollo_federation::composition::Satisfiable;
use apollo_federation::supergraph::Supergraph;

use super::ServiceDefinition;
use super::assert_composition_errors;
use super::compose_as_fed2_subgraphs;

/// Validates that @tag directives are properly propagated to the supergraph schema
/// Equivalent to validatePropagation function in the JS tests
fn validate_tag_propagation(supergraph: &Supergraph<Satisfiable>) {
    let schema = supergraph.schema().schema();

    // Check for @tag directive on Query.users field
    if let Some(query_type_name) = &schema.schema_definition.query
        && let Some(ExtendedType::Object(query_obj)) = schema.types.get(query_type_name.as_str())
        && let Some(users_field) = query_obj.fields.get("users")
    {
        let tag_directives: Vec<_> = users_field
            .directives
            .iter()
            .filter(|d| d.name == "tag")
            .collect();
        assert!(
            !tag_directives.is_empty(),
            "Expected @tag directive on Query.users field"
        );

        // Check for the specific tag name "aTaggedOperation"
        let has_operation_tag = tag_directives.iter().any(|d| {
            d.arguments
                .iter()
                .any(|arg| arg.name == "name" && arg.value.to_string().contains("aTaggedOperation"))
        });
        assert!(
            has_operation_tag,
            "Expected @tag(name: \"aTaggedOperation\") on Query.users"
        );
    }

    // Check for @tag directive on User type
    if let Some(ExtendedType::Object(user_type)) = schema.types.get("User") {
        let tag_directives: Vec<_> = user_type
            .directives
            .iter()
            .filter(|d| d.name == "tag")
            .collect();
        assert!(
            !tag_directives.is_empty(),
            "Expected @tag directive on User type"
        );

        // Check for the specific tag name "aTaggedType"
        let has_type_tag = tag_directives.iter().any(|d| {
            d.arguments
                .iter()
                .any(|arg| arg.name == "name" && arg.value.to_string().contains("aTaggedType"))
        });
        assert!(
            has_type_tag,
            "Expected @tag(name: \"aTaggedType\") on User type"
        );

        // Check for @tag directive on User.name field
        if let Some(name_field) = user_type.fields.get("name") {
            let field_tag_directives: Vec<_> = name_field
                .directives
                .iter()
                .filter(|d| d.name == "tag")
                .collect();
            assert!(
                !field_tag_directives.is_empty(),
                "Expected @tag directive on User.name field"
            );

            // Check for the specific tag name "aTaggedField"
            let has_field_tag = field_tag_directives.iter().any(|d| {
                d.arguments
                    .iter()
                    .any(|arg| arg.name == "name" && arg.value.to_string().contains("aTaggedField"))
            });
            assert!(
                has_field_tag,
                "Expected @tag(name: \"aTaggedField\") on User.name field"
            );
        }
    }
}

/// Validates that multiple @tag directives are properly merged
/// Equivalent to the validatePropagation function for merging tests in JS
fn validate_tag_merging(supergraph: &Supergraph<Satisfiable>) {
    let schema = supergraph.schema().schema();

    // Check merged tags on User type
    if let Some(ExtendedType::Object(user_type)) = schema.types.get("User") {
        let tag_directives: Vec<_> = user_type
            .directives
            .iter()
            .filter(|d| d.name == "tag")
            .collect();

        // Should have multiple @tag directives merged
        assert!(
            tag_directives.len() >= 2,
            "Expected multiple @tag directives on User type, got {}",
            tag_directives.len()
        );

        // Extract tag names for validation
        let tag_names: Vec<String> = tag_directives
            .iter()
            .filter_map(|d| {
                d.arguments.iter().find_map(|arg| {
                    if arg.name == "name" {
                        Some(arg.value.to_string().trim_matches('"').to_string())
                    } else {
                        None
                    }
                })
            })
            .collect();

        // Should contain tags from both subgraphs plus merged tag
        assert!(
            tag_names.contains(&"aTagOnTypeFromSubgraphA".to_string()),
            "Missing aTagOnTypeFromSubgraphA"
        );
        assert!(
            tag_names.contains(&"aMergedTagOnType".to_string()),
            "Missing aMergedTagOnType"
        );
        assert!(
            tag_names.contains(&"aTagOnTypeFromSubgraphB".to_string()),
            "Missing aTagOnTypeFromSubgraphB"
        );
    }

    // Check merged tags on Name.firstName field
    if let Some(ExtendedType::Object(name_type)) = schema.types.get("Name")
        && let Some(first_name_field) = name_type.fields.get("firstName")
    {
        let field_tag_directives: Vec<_> = first_name_field
            .directives
            .iter()
            .filter(|d| d.name == "tag")
            .collect();

        assert!(
            field_tag_directives.len() >= 2,
            "Expected multiple @tag directives on Name.firstName field"
        );
    }
}

// =============================================================================
// @tag DIRECTIVE PROPAGATION - Tests for @tag propagation to supergraph
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
fn tag_propagates_to_supergraph_fed2_subgraphs() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          users: [User] @tag(name: "aTaggedOperation")
        }

        type User @key(fields: "id") {
          id: ID!
          name: String! @tag(name: "aTaggedField")
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User @key(fields: "id") @tag(name: "aTaggedType") {
          id: ID!
          birthdate: String!
          age: Int!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");

    // Validate that @tag directives are properly propagated to the supergraph schema
    validate_tag_propagation(&supergraph);
}

#[test]
#[ignore = "until Fed1 composition mode is implemented"]
fn tag_propagates_to_supergraph_fed1_subgraphs() {
    let _subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          users: [User] @tag(name: "aTaggedOperation")
        }

        type User @key(fields: "id") {
          id: ID!
          name: String! @tag(name: "aTaggedField")
        }
        "#,
    };

    let _subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User @key(fields: "id") @tag(name: "aTaggedType") {
          id: ID!
          birthdate: String!
          age: Int!
        }
        "#,
    };

    // TODO: Implement Fed1 composition mode - this should use composeServices() equivalent
    panic!(
        "Fed1 composition mode not yet implemented - need compose_services() function equivalent to JS composeServices([subgraphA, subgraphB])"
    );
}

#[test]
#[ignore = "until mixed Fed1/Fed2 composition mode is implemented"]
fn tag_propagates_to_supergraph_mixed_fed1_fed2_subgraphs() {
    let _subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          users: [User] @tag(name: "aTaggedOperation")
        }

        type User @key(fields: "id") {
          id: ID!
          name: String! @tag(name: "aTaggedField")
        }
        "#,
    };

    let _subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User @key(fields: "id") @tag(name: "aTaggedType") {
          id: ID!
          birthdate: String!
          age: Int!
        }
        "#,
    };

    // TODO: Implement mixed Fed1/Fed2 composition mode - this should use composeServices([subgraphA, asFed2Service(subgraphB)]) equivalent
    panic!(
        "Mixed Fed1/Fed2 composition mode not yet implemented - need compose_services() function equivalent to JS composeServices([subgraphA, asFed2Service(subgraphB)])"
    );
}

// =============================================================================
// @tag DIRECTIVE MERGING - Tests for merging multiple @tag directives
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
fn tag_merges_multiple_tags_fed2_subgraphs() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          user: [User]
        }

        type User @key(fields: "id") @tag(name: "aTagOnTypeFromSubgraphA") @tag(name: "aMergedTagOnType") {
          id: ID!
          name1: Name!
        }

        type Name {
          firstName: String @tag(name: "aTagOnFieldFromSubgraphA")
          lastName: String @tag(name: "aMergedTagOnField")
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User @key(fields: "id") @tag(name: "aTagOnTypeFromSubgraphB") @tag(name: "aMergedTagOnType") {
          id: ID!
          name2: String!
        }

        type Name {
          firstName: String @tag(name: "aTagOnFieldFromSubgraphB")
          lastName: String @tag(name: "aMergedTagOnField")
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");

    // Validate that multiple @tag directives are properly merged
    validate_tag_merging(&supergraph);
}

#[test]
#[ignore = "until Fed1 composition mode is implemented"]
fn tag_merges_multiple_tags_fed1_subgraphs() {
    let _subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          user: [User]
        }

        type User @key(fields: "id") @tag(name: "aTagOnTypeFromSubgraphA") @tag(name: "aMergedTagOnType") {
          id: ID!
          name1: Name!
        }

        type Name {
          firstName: String @tag(name: "aTagOnFieldFromSubgraphA")
          lastName: String @tag(name: "aMergedTagOnField")
        }
        "#,
    };

    let _subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User @key(fields: "id") @tag(name: "aTagOnTypeFromSubgraphB") @tag(name: "aMergedTagOnType") {
          id: ID!
          name2: String!
        }

        type Name {
          firstName: String @tag(name: "aTagOnFieldFromSubgraphB")
          lastName: String @tag(name: "aMergedTagOnField")
        }
        "#,
    };

    // TODO: Implement Fed1 composition mode - this should use composeServices() equivalent
    panic!(
        "Fed1 composition mode not yet implemented - need compose_services() function equivalent to JS composeServices([subgraphA, subgraphB])"
    );
}

#[test]
#[ignore = "until mixed Fed1/Fed2 composition mode is implemented"]
fn tag_merges_multiple_tags_mixed_fed1_fed2_subgraphs() {
    let _subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          user: [User]
        }

        type User @key(fields: "id") @tag(name: "aTagOnTypeFromSubgraphA") @tag(name: "aMergedTagOnType") {
          id: ID!
          name1: Name!
        }

        type Name {
          firstName: String @tag(name: "aTagOnFieldFromSubgraphA")
          lastName: String @tag(name: "aMergedTagOnField")
        }
        "#,
    };

    let _subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User @key(fields: "id") @tag(name: "aTagOnTypeFromSubgraphB") @tag(name: "aMergedTagOnType") {
          id: ID!
          name2: String!
        }

        type Name @shareable {
          firstName: String @tag(name: "aTagOnFieldFromSubgraphB")
          lastName: String @tag(name: "aMergedTagOnField")
        }
        "#,
    };

    // TODO: Implement mixed Fed1/Fed2 composition mode with proper shareable handling
    // This should use composeServices([subgraphA, updatedSubgraphB]) equivalent
    // where subgraphB has been converted to Fed2 with @shareable applied to Name type
    panic!(
        "Mixed Fed1/Fed2 composition mode not yet implemented - need compose_services() function equivalent to JS composeServices([subgraphA, updatedSubgraphB])"
    );
}

// =============================================================================
// @tag DIRECTIVE VALIDATION - Tests for @tag and @external conflicts
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
fn tag_rejects_tag_and_external_together_fed2_subgraphs() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          user: [User]
        }

        type User @key(fields: "id") {
          id: ID!
          name: String!
          birthdate: Int! @external @tag(name: "myTag")
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
            r#"[subgraphA] Cannot apply merged directive @tag(name: "myTag") to external field "User.birthdate""#,
        )],
    );
}

#[test]
#[ignore = "until Fed1 composition mode is implemented"]
fn tag_rejects_tag_and_external_together_fed1_subgraphs() {
    let _subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          user: [User]
        }

        type User @key(fields: "id") {
          id: ID!
          name: String!
          birthdate: Int! @external @tag(name: "myTag")
          age: Int! @requires(fields: "birthdate")
        }
        "#,
    };

    let _subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User @key(fields: "id") {
          id: ID!
          birthdate: Int!
        }
        "#,
    };

    // TODO: Implement Fed1 composition mode - this should use composeServices() equivalent
    // and should produce MERGED_DIRECTIVE_APPLICATION_ON_EXTERNAL error
    panic!(
        "Fed1 composition mode not yet implemented - need compose_services() function equivalent to JS composeServices([subgraphA, subgraphB]) that validates @tag+@external conflicts"
    );
}

#[test]
#[ignore = "until mixed Fed1/Fed2 composition mode is implemented"]
fn tag_rejects_tag_and_external_together_mixed_fed1_fed2_subgraphs() {
    let _subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          user: [User]
        }

        type User @key(fields: "id") {
          id: ID!
          name: String!
          birthdate: Int! @external @tag(name: "myTag")
          age: Int! @requires(fields: "birthdate")
        }
        "#,
    };

    let _subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User @key(fields: "id") {
          id: ID!
          birthdate: Int!
        }
        "#,
    };

    // TODO: Implement mixed Fed1/Fed2 composition mode - this should use composeServices([subgraphA, asFed2Service(subgraphB)]) equivalent
    // and should produce MERGED_DIRECTIVE_APPLICATION_ON_EXTERNAL error
    panic!(
        "Mixed Fed1/Fed2 composition mode not yet implemented - need compose_services() function equivalent to JS composeServices([subgraphA, asFed2Service(subgraphB)]) that validates @tag+@external conflicts"
    );
}

// =============================================================================
// @tag DIRECTIVE IMPORT VALIDATION - Tests for @tag import name validation
// =============================================================================

#[test]
#[ignore = "until merge implementation completed"]
fn tag_errors_if_imported_under_mismatched_names() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@tag", as: "@apolloTag"}])

        type Query {
          q1: Int @apolloTag(name: "t1")
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@tag"])

        type Query {
          q2: Int @tag(name: "t2")
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "LINK_IMPORT_NAME_MISMATCH",
            r#"The "@tag" directive (from https://specs.apollo.dev/federation/v2.0) is imported with mismatched name between subgraphs: it is imported as "@tag" in subgraph "subgraphB" but "@apolloTag" in subgraph "subgraphA""#,
        )],
    );
}

#[test]
#[ignore = "until merge implementation completed"]
fn tag_succeeds_if_imported_under_same_non_default_name() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@tag", as: "@apolloTag"}])

        type Query {
          q1: Int @apolloTag(name: "t1")
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@tag", as: "@apolloTag"}])

        type Query {
          q2: Int @apolloTag(name: "t2")
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let _supergraph =
        result.expect("Expected composition to succeed with consistent @tag import names");
}
