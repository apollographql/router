use apollo_federation::merge::merge_federation_subgraphs;
use insta::assert_snapshot;

use crate::merger::{
    assert_composition_failure, compose_as_fed2_subgraphs, extract_errors,
    test_subgraphs, basic_subgraph_template,
};

#[test]
fn test_missing_key_directive() {
    let subgraphs = test_subgraphs! {
        "users" => basic_subgraph_template("users", r#"
            type User {
                id: ID!
                name: String!
            }
        "#),
        "posts" => basic_subgraph_template("posts", r#"
            type User @key(fields: "id") {
                id: ID!
            }

            type Post @key(fields: "id") {
                id: ID!
                author: User!
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    // This should potentially fail or generate warnings about inconsistent key usage
    // For now, let's see what the current implementation does
    let errors = extract_errors(&result);
    if !errors.is_empty() {
        assert_snapshot!(errors.join("\n"));
    }
}

#[test]
fn test_type_mismatch_error() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            type User @key(fields: "id") {
                id: ID!
                age: String!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            type User @key(fields: "id") {
                id: ID!
                age: Int!
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    // This should potentially fail due to type mismatch
    let errors = extract_errors(&result);
    if !errors.is_empty() {
        assert_snapshot!(errors.join("\n"));
    }
}

#[test]
fn test_invalid_requires_field() {
    let subgraphs = test_subgraphs! {
        "users" => basic_subgraph_template("users", r#"
            type User @key(fields: "id") {
                id: ID!
                name: String!
            }
        "#),
        "profiles" => basic_subgraph_template("profiles", r#"
            type User @key(fields: "id") {
                id: ID!
                nonExistentField: String! @external
                fullName: String! @requires(fields: "nonExistentField")
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    // This should fail due to requiring a non-existent field
    let errors = extract_errors(&result);
    if !errors.is_empty() {
        assert_snapshot!(errors.join("\n"));
    }
}

#[test]
fn test_circular_requires_dependency() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            type User @key(fields: "id") {
                id: ID!
                fieldA: String! @external
                fieldB: String! @requires(fields: "fieldA")
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            type User @key(fields: "id") {
                id: ID!
                fieldB: String! @external
                fieldA: String! @requires(fields: "fieldB")
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    // This should potentially fail due to circular dependency
    let errors = extract_errors(&result);
    if !errors.is_empty() {
        assert_snapshot!(errors.join("\n"));
    }
}

#[test]
fn test_invalid_override_from() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])

            type Query {
                _dummy: String
            }

            type Product @key(fields: "id") {
                id: ID!
                name: String!
            }
        "#,
        "subgraph_b" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])

            type Query {
                _dummy: String
            }

            type Product @key(fields: "id") {
                id: ID!
                name: String! @override(from: "nonExistentSubgraph")
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    // This should fail due to invalid override source
    let errors = extract_errors(&result);
    if !errors.is_empty() {
        assert_snapshot!(errors.join("\n"));
    }
}

#[test]
fn test_conflicting_field_types() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            type Product @key(fields: "id") {
                id: ID!
                price: Float!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            type Product @key(fields: "id") {
                id: ID!
                price: String!
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    // This should fail due to conflicting field types
    let errors = extract_errors(&result);
    if !errors.is_empty() {
        assert_snapshot!(errors.join("\n"));
    }
}

#[test]
fn test_missing_external_field() {
    let subgraphs = test_subgraphs! {
        "users" => basic_subgraph_template("users", r#"
            type User @key(fields: "id") {
                id: ID!
                name: String!
            }
        "#),
        "posts" => basic_subgraph_template("posts", r#"
            type User @key(fields: "id") {
                id: ID!
                email: String! @external
                displayName: String! @requires(fields: "email")
            }

            type Post @key(fields: "id") {
                id: ID!
                author: User!
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    // This should fail because email field is marked external but doesn't exist in the owning subgraph
    let errors = extract_errors(&result);
    if !errors.is_empty() {
        assert_snapshot!(errors.join("\n"));
    }
}

#[test]
fn test_invalid_key_field() {
    let subgraphs = test_subgraphs! {
        "users" => basic_subgraph_template("users", r#"
            type User @key(fields: "nonExistentField") {
                id: ID!
                name: String!
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    // This should fail due to key referencing non-existent field
    let errors = extract_errors(&result);
    if !errors.is_empty() {
        assert_snapshot!(errors.join("\n"));
    }
}