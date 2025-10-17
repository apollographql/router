use apollo_compiler::coord;
use apollo_compiler::schema::ExtendedType;
use apollo_federation::composition::Satisfiable;
use apollo_federation::composition::compose;
use apollo_federation::subgraph::typestate::Subgraph;
use apollo_federation::supergraph::Supergraph;
use itertools::Itertools;

use super::ServiceDefinition;
use super::assert_composition_errors;
use super::compose_as_fed2_subgraphs;

/// Validates that @tag directives are properly propagated to the supergraph schema
/// Equivalent to validatePropagation function in the JS tests
fn validate_tag_propagation(supergraph: &Supergraph<Satisfiable>) {
    let schema = supergraph.schema().schema();

    // Check for @tag directive on Query.users field
    let users_field = coord!(Query.users)
        .lookup_field(schema)
        .expect("Query.users should exist");
    let users_field_tags = users_field
        .directives
        .iter()
        .filter(|d| d.name == "tag")
        .join(" ");
    assert_eq!(
        users_field_tags, r#"@tag(name: "aTaggedOperation")"#,
        "Query.users should have correct @tag directive"
    );

    // Check for @tag directive on User type
    let user_type = coord!(User)
        .lookup(schema)
        .expect("User type should exist")
        .as_object()
        .expect("User should be an object type");
    let user_type_tags = user_type
        .directives
        .iter()
        .filter(|d| d.name == "tag")
        .map(|d| d.to_string())
        .join(" ");
    assert_eq!(
        user_type_tags, r#"@tag(name: "aTaggedType")"#,
        "User type should have correct @tag directive"
    );

    // Check for @tag directive on User.name field
    let user_name_field = coord!(User.name)
        .lookup_field(schema)
        .expect("User.name field should exist");
    let user_name_field_tags = user_name_field
        .directives
        .iter()
        .filter(|d| d.name == "tag")
        .join(" ");
    assert_eq!(
        user_name_field_tags, r#"@tag(name: "aTaggedField")"#,
        "User.name should have correct @tag directive"
    );
}

/// Validates that multiple @tag directives are properly merged
/// Equivalent to the validatePropagation function for merging tests in JS
fn validate_tag_merging(supergraph: &Supergraph<Satisfiable>) {
    let schema = supergraph.schema().schema();

    // Check merged tags on User type
    let user_type = coord!(User)
        .lookup(schema)
        .expect("User type should exist")
        .as_object()
        .expect("User should be an object type");
    let user_type_tags = user_type
        .directives
        .iter()
        .filter(|d| d.name == "tag")
        .map(|d| d.to_string())
        .join(" ");
    assert_eq!(
        user_type_tags,
        r#"@tag(name: "aTagOnTypeFromSubgraphA") @tag(name: "aMergedTagOnType") @tag(name: "aTagOnTypeFromSubgraphB")"#,
        "User type should have merged @tag directives"
    );

    // Check merged tags on Name.firstName field
    let first_name_field = coord!(Name.firstName)
        .lookup_field(schema)
        .expect("Name.firstName field should exist");
    let field_tag_directives = first_name_field
        .directives
        .iter()
        .filter(|d| d.name == "tag")
        .join(" ");
    assert_eq!(
        field_tag_directives,
        r#"@tag(name: "aTagOnFieldFromSubgraphA") @tag(name: "aTagOnFieldFromSubgraphB")"#,
        "Name.firstName should have merged @tag directives"
    );
}

// =============================================================================
// @tag DIRECTIVE PROPAGATION - Tests for @tag propagation to supergraph
// =============================================================================

#[test]
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
fn tag_propagates_to_supergraph_fed1_subgraphs() {
    let subgraph_a = Subgraph::parse(
        "subgraphA",
        "",
        r#"
        type Query {
          users: [User] @tag(name: "aTaggedOperation")
        }

        type User @key(fields: "id") {
          id: ID!
          name: String! @tag(name: "aTaggedField")
        }
        "#,
    )
    .unwrap();

    let subgraph_b = Subgraph::parse(
        "subgraphB",
        "",
        r#"
        type User @key(fields: "id") @tag(name: "aTaggedType") {
          id: ID!
          birthdate: String!
          age: Int!
        }
        "#,
    )
    .unwrap();

    let supergraph =
        compose(vec![subgraph_a, subgraph_b]).expect("Expected composition to succeed");
    validate_tag_propagation(&supergraph);
}

#[test]
fn tag_propagates_to_supergraph_mixed_fed1_fed2_subgraphs() {
    let subgraph_a = Subgraph::parse(
        "subgraphA",
        "",
        r#"
        type Query {
          users: [User] @tag(name: "aTaggedOperation")
        }

        type User @key(fields: "id") {
          id: ID!
          name: String! @tag(name: "aTaggedField")
        }
        "#,
    )
    .unwrap();

    let subgraph_b = Subgraph::parse(
        "subgraphB",
        "",
        r#"
        type User @key(fields: "id") @tag(name: "aTaggedType") {
          id: ID!
          birthdate: String!
          age: Int!
        }
        "#,
    )
    .unwrap()
    .into_fed2_test_subgraph(true, false)
    .unwrap();

    let supergraph =
        compose(vec![subgraph_a, subgraph_b]).expect("Expected composition to succeed");
    validate_tag_propagation(&supergraph);
}

// =============================================================================
// @tag DIRECTIVE MERGING - Tests for merging multiple @tag directives
// =============================================================================

#[test]
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

        type Name @shareable {
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

        type Name @shareable {
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
fn tag_merges_multiple_tags_fed1_subgraphs() {
    let subgraph_a = Subgraph::parse("subgraphA", "", 
        r#"
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
    ).unwrap();

    let subgraph_b = Subgraph::parse( "subgraphB", "",
        r#"
        type User @key(fields: "id") @tag(name: "aTagOnTypeFromSubgraphB") @tag(name: "aMergedTagOnType") {
          id: ID!
          name2: String!
        }

        type Name {
          firstName: String @tag(name: "aTagOnFieldFromSubgraphB")
          lastName: String @tag(name: "aMergedTagOnField")
        }
        "#,
    ).unwrap();

    let supergraph =
        compose(vec![subgraph_a, subgraph_b]).expect("Expected composition to succeed");
    validate_tag_merging(&supergraph);
}

#[test]
fn tag_merges_multiple_tags_mixed_fed1_fed2_subgraphs() {
    let subgraph_a = Subgraph::parse("subgraphA", "", 
        r#"
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
    ).unwrap();

    let subgraph_b = Subgraph::parse( "subgraphB", "",
        r#"
        type User @key(fields: "id") @tag(name: "aTagOnTypeFromSubgraphB") @tag(name: "aMergedTagOnType") {
          id: ID!
          name2: String!
        }

        type Name @shareable {
          firstName: String @tag(name: "aTagOnFieldFromSubgraphB")
          lastName: String @tag(name: "aMergedTagOnField")
        }
        "#,
    ).unwrap().into_fed2_test_subgraph(true, false).unwrap();

    let supergraph =
        compose(vec![subgraph_a, subgraph_b]).expect("Expected composition to succeed");
    validate_tag_merging(&supergraph);
}

// =============================================================================
// @tag DIRECTIVE VALIDATION - Tests for @tag and @external conflicts
// =============================================================================

#[test]
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
fn tag_rejects_tag_and_external_together_fed1_subgraphs() {
    let subgraph_a = Subgraph::parse(
        "subgraphA",
        "",
        r#"
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
    )
    .unwrap();

    let subgraph_b = Subgraph::parse(
        "subgraphB",
        "",
        r#"
        type User @key(fields: "id") {
          id: ID!
          birthdate: Int!
        }
        "#,
    )
    .unwrap();

    let result = compose(vec![subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "MERGED_DIRECTIVE_APPLICATION_ON_EXTERNAL",
            r#"[subgraphA] Cannot apply merged directive @tag(name: "myTag") to external field "User.birthdate""#,
        )],
    );
}

#[test]
fn tag_rejects_tag_and_external_together_mixed_fed1_fed2_subgraphs() {
    let subgraph_a = Subgraph::parse(
        "subgraphA",
        "",
        r#"
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
    )
    .unwrap();

    let subgraph_b = Subgraph::parse(
        "subgraphB",
        "",
        r#"
        type User @key(fields: "id") {
          id: ID!
          birthdate: Int!
        }
        "#,
    )
    .unwrap()
    .into_fed2_test_subgraph(true, false)
    .unwrap();

    let result = compose(vec![subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "MERGED_DIRECTIVE_APPLICATION_ON_EXTERNAL",
            r#"[subgraphA] Cannot apply merged directive @tag(name: "myTag") to external field "User.birthdate""#,
        )],
    );
}

// =============================================================================
// @tag DIRECTIVE IMPORT VALIDATION - Tests for @tag import name validation
// =============================================================================

#[test]
fn tag_errors_if_imported_under_mismatched_names() {
    let subgraph_a = Subgraph::parse(
        "subgraphA",
        "",
        r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@tag", as: "@apolloTag"}])

        type Query {
          q1: Int @apolloTag(name: "t1")
        }
        "#,
    )
    .unwrap();

    let subgraph_b = Subgraph::parse(
        "subgraphB",
        "",
        r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@tag"])

        type Query {
          q2: Int @tag(name: "t2")
        }
        "#,
    )
    .unwrap();

    let result = compose(vec![subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "LINK_IMPORT_NAME_MISMATCH",
            r#"The "@tag" directive (from https://specs.apollo.dev/federation/v2.0) is imported with mismatched name between subgraphs: it is imported as "@tag" in subgraph "subgraphB" but "@apolloTag" in subgraph "subgraphA""#,
        )],
    );
}

#[test]
fn tag_succeeds_if_imported_under_same_non_default_name() {
    let subgraph_a = Subgraph::parse(
        "subgraphA",
        "",
        r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@tag", as: "@apolloTag"}])

        type Query {
          q1: Int @apolloTag(name: "t1")
        }
        "#,
    )
    .unwrap();

    let subgraph_b = Subgraph::parse(
        "subgraphB",
        "",
        r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@tag", as: "@apolloTag"}])

        type Query {
          q2: Int @apolloTag(name: "t2")
        }
        "#,
    )
    .unwrap();

    let result = compose(vec![subgraph_a, subgraph_b]);
    let supergraph =
        result.expect("Expected composition to succeed with consistent @tag import names");

    let q1 = coord!(Query.q1)
        .lookup_field(supergraph.schema().schema())
        .unwrap();
    assert!(
        q1.directives
            .iter()
            .any(|d| d.to_string() == r#"@apolloTag(name: "t1")"#)
    );
    let q2 = coord!(Query.q2)
        .lookup_field(supergraph.schema().schema())
        .unwrap();
    assert!(
        q2.directives
            .iter()
            .any(|d| d.to_string() == r#"@apolloTag(name: "t2")"#)
    );
}
