use apollo_federation::merge::merge_federation_subgraphs;
use insta::assert_snapshot;

use crate::merger::{
    assert_composition_success, assert_composition_failure, compose_as_fed2_subgraphs,
    extract_schemas, serialize_schema, test_subgraphs, basic_subgraph_template,
};

#[test]
fn test_valid_entity_extension() {
    let subgraphs = test_subgraphs! {
        "users" => basic_subgraph_template("users", r#"
            type User @key(fields: "id") {
                id: ID!
                name: String!
                email: String!
            }
        "#),
        "profiles" => basic_subgraph_template("profiles", r#"
            extend type User @key(fields: "id") {
                id: ID! @external
                profile: UserProfile!
            }

            type UserProfile {
                bio: String
                avatar: String
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    assert_composition_success!(result);

    let success = result.unwrap();
    let (schema, _hints) = extract_schemas(&success);
    let schema_sdl = serialize_schema(schema);
    
    assert_snapshot!(schema_sdl);
}

#[test]
fn test_interface_implementation_validation() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            interface Node {
                id: ID!
            }

            type User implements Node @key(fields: "id") {
                id: ID!
                name: String!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            interface Node {
                id: ID!
            }

            type User implements Node @key(fields: "id") {
                id: ID!
                email: String!
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    assert_composition_success!(result);

    let success = result.unwrap();
    let (schema, _hints) = extract_schemas(&success);
    let schema_sdl = serialize_schema(schema);
    
    assert_snapshot!(schema_sdl);
}

#[test]
fn test_field_argument_validation() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            type User @key(fields: "id") {
                id: ID!
                posts(first: Int!, after: String): [Post!]!
            }

            type Post {
                id: ID!
                title: String!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            type User @key(fields: "id") {
                id: ID!
                posts(first: Int!, after: String): [Post!]!
            }

            type Post {
                id: ID!
                content: String!
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    assert_composition_success!(result);

    let success = result.unwrap();
    let (schema, _hints) = extract_schemas(&success);
    let schema_sdl = serialize_schema(schema);
    
    assert_snapshot!(schema_sdl);
}

#[test]
fn test_enum_value_consistency() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            enum Status {
                ACTIVE
                INACTIVE
                PENDING
            }

            type User @key(fields: "id") {
                id: ID!
                status: Status!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            enum Status {
                ACTIVE
                INACTIVE
                PENDING
            }

            type Product @key(fields: "id") {
                id: ID!
                status: Status!
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    assert_composition_success!(result);

    let success = result.unwrap();
    let (schema, _hints) = extract_schemas(&success);
    let schema_sdl = serialize_schema(schema);
    
    assert_snapshot!(schema_sdl);
}

#[test]
fn test_input_type_validation() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            input UserInput {
                name: String!
                email: String!
                age: Int
            }

            type Query {
                createUser(input: UserInput!): User
            }

            type User @key(fields: "id") {
                id: ID!
                name: String!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            input UserInput {
                name: String!
                email: String!
                age: Int
            }

            type User @key(fields: "id") {
                id: ID!
                email: String!
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    assert_composition_success!(result);

    let success = result.unwrap();
    let (schema, _hints) = extract_schemas(&success);
    let schema_sdl = serialize_schema(schema);
    
    assert_snapshot!(schema_sdl);
}

#[test]
fn test_directive_definition_merging() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            directive @customDirective(value: String!) on FIELD_DEFINITION

            type User @key(fields: "id") {
                id: ID!
                name: String! @customDirective(value: "test")
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            directive @customDirective(value: String!) on FIELD_DEFINITION

            type Product @key(fields: "id") {
                id: ID!
                title: String! @customDirective(value: "product")
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    assert_composition_success!(result);

    let success = result.unwrap();
    let (schema, _hints) = extract_schemas(&success);
    let schema_sdl = serialize_schema(schema);
    
    assert_snapshot!(schema_sdl);
}

#[test]
fn test_schema_definition_merging() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

            type Query {
                users: [User!]!
            }

            type User @key(fields: "id") {
                id: ID!
                name: String!
            }
        "#,
        "subgraph_b" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

            type Query {
                products: [Product!]!
            }

            type Mutation {
                createProduct(name: String!): Product
            }

            type Product @key(fields: "id") {
                id: ID!
                name: String!
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    assert_composition_success!(result);

    let success = result.unwrap();
    let (schema, _hints) = extract_schemas(&success);
    let schema_sdl = serialize_schema(schema);
    
    // Should have both Query and Mutation types
    assert!(schema_sdl.contains("type Query"));
    assert!(schema_sdl.contains("type Mutation"));
    assert_snapshot!(schema_sdl);
}

#[test]
fn test_complex_validation_scenario() {
    let subgraphs = test_subgraphs! {
        "accounts" => basic_subgraph_template("accounts", r#"
            interface Node {
                id: ID!
            }

            type User implements Node @key(fields: "id") @key(fields: "email") {
                id: ID!
                email: String!
                username: String!
            }
        "#),
        "profiles" => basic_subgraph_template("profiles", r#"
            interface Node {
                id: ID!
            }

            type User implements Node @key(fields: "id") {
                id: ID!
                profile: UserProfile!
            }

            type UserProfile {
                displayName: String!
                bio: String
                preferences: UserPreferences!
            }

            type UserPreferences {
                theme: Theme!
                language: String!
            }

            enum Theme {
                LIGHT
                DARK
                AUTO
            }
        "#),
        "social" => basic_subgraph_template("social", r#"
            type User @key(fields: "email") {
                email: String!
                followers: [User!]!
                following: [User!]!
                posts: [Post!]!
            }

            type Post @key(fields: "id") {
                id: ID!
                content: String!
                author: User!
                likes: Int!
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    assert_composition_success!(result);

    let success = result.unwrap();
    let (schema, _hints) = extract_schemas(&success);
    let schema_sdl = serialize_schema(schema);
    
    assert_snapshot!(schema_sdl);
}