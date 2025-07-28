use apollo_federation::merge::merge_federation_subgraphs;
use insta::assert_snapshot;

use crate::merger::{
    assert_composition_success, compose_as_fed2_subgraphs, extract_schemas, serialize_schema,
    test_subgraphs, basic_subgraph_template,
};

#[test]
fn test_basic_composition_success() {
    let subgraphs = test_subgraphs! {
        "users" => basic_subgraph_template("users", r#"
            type User @key(fields: "id") {
                id: ID!
                name: String!
            }
        "#),
        "products" => basic_subgraph_template("products", r#"
            type Product @key(fields: "id") {
                id: ID!
                title: String!
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
fn test_composition_with_shared_types() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            type Query {
                users: [User!]!
            }

            type User @key(fields: "id") {
                id: ID!
                name: String!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
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
    
    // Verify that both fields are present in the merged User type
    assert!(schema_sdl.contains("name: String"));
    assert!(schema_sdl.contains("email: String"));
    assert_snapshot!(schema_sdl);
}

#[test]
fn test_composition_with_interfaces() {
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

            type Product implements Node @key(fields: "id") {
                id: ID!
                title: String!
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
fn test_composition_with_unions() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            union SearchResult = User | Product

            type User @key(fields: "id") {
                id: ID!
                name: String!
            }

            type Product @key(fields: "id") {
                id: ID!
                title: String!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            union SearchResult = User | Article

            type User @key(fields: "id") {
                id: ID!
                email: String!
            }

            type Article @key(fields: "id") {
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
fn test_composition_with_enums() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            enum Status {
                ACTIVE
                INACTIVE
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
fn test_composition_with_scalars() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            scalar DateTime

            type User @key(fields: "id") {
                id: ID!
                createdAt: DateTime!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            scalar DateTime

            type Product @key(fields: "id") {
                id: ID!
                updatedAt: DateTime!
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
fn test_composition_with_input_types() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            input UserInput {
                name: String!
                email: String!
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
fn test_composition_with_multiple_keys() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            type User @key(fields: "id") @key(fields: "email") {
                id: ID!
                email: String!
                name: String!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            type User @key(fields: "id") @key(fields: "username") {
                id: ID!
                username: String!
                profile: String!
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
fn test_composition_with_nested_types() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            type User @key(fields: "id") {
                id: ID!
                profile: UserProfile!
            }

            type UserProfile {
                firstName: String!
                lastName: String!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            type User @key(fields: "id") {
                id: ID!
                settings: UserSettings!
            }

            type UserSettings {
                theme: String!
                notifications: Boolean!
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
fn test_composition_with_field_arguments() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            type Query {
                user(id: ID!): User
                users(limit: Int = 10, offset: Int = 0): [User!]!
            }

            type User @key(fields: "id") {
                id: ID!
                name: String!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            type User @key(fields: "id") {
                id: ID!
                posts(first: Int, after: String): [Post!]!
            }

            type Post {
                id: ID!
                title: String!
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