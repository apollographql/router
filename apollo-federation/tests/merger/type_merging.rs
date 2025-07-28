use apollo_federation::merge::merge_federation_subgraphs;
use insta::assert_snapshot;

use crate::merger::{
    assert_composition_success, assert_composition_failure, compose_as_fed2_subgraphs,
    extract_schemas, serialize_schema, test_subgraphs, basic_subgraph_template,
};

#[test]
fn test_field_type_compatibility_success() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            type User @key(fields: "id") {
                id: ID!
                name: String!
                age: Int
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            type User @key(fields: "id") {
                id: ID!
                name: String!
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
fn test_nullable_vs_non_nullable_fields() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            type User @key(fields: "id") {
                id: ID!
                name: String!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            type User @key(fields: "id") {
                id: ID!
                name: String
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    // This should succeed - nullable is more permissive than non-nullable
    assert_composition_success!(result);

    let success = result.unwrap();
    let (schema, _hints) = extract_schemas(&success);
    let schema_sdl = serialize_schema(schema);
    
    assert_snapshot!(schema_sdl);
}

#[test]
fn test_list_type_compatibility() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            type User @key(fields: "id") {
                id: ID!
                tags: [String!]!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            type User @key(fields: "id") {
                id: ID!
                tags: [String!]!
                categories: [String]
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
fn test_interface_implementation_merging() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            interface Node {
                id: ID!
            }

            interface Timestamped {
                createdAt: String!
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

            interface Timestamped {
                createdAt: String!
                updatedAt: String!
            }

            type User implements Node & Timestamped @key(fields: "id") {
                id: ID!
                createdAt: String!
                updatedAt: String!
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
fn test_union_member_merging() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            union Content = Article | Video

            type Article @key(fields: "id") {
                id: ID!
                title: String!
            }

            type Video @key(fields: "id") {
                id: ID!
                duration: Int!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            union Content = Article | Image

            type Article @key(fields: "id") {
                id: ID!
                content: String!
            }

            type Image @key(fields: "id") {
                id: ID!
                url: String!
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    assert_composition_success!(result);

    let success = result.unwrap();
    let (schema, _hints) = extract_schemas(&success);
    let schema_sdl = serialize_schema(schema);
    
    // Union should contain all members from both subgraphs
    assert!(schema_sdl.contains("Article"));
    assert!(schema_sdl.contains("Video"));
    assert!(schema_sdl.contains("Image"));
    assert_snapshot!(schema_sdl);
}

#[test]
fn test_field_argument_merging() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            type User @key(fields: "id") {
                id: ID!
                posts(limit: Int = 10): [Post!]!
            }

            type Post {
                id: ID!
                title: String!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            type User @key(fields: "id") {
                id: ID!
                posts(limit: Int = 10, offset: Int = 0): [Post!]!
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
fn test_enum_value_merging() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            enum Status {
                DRAFT
                PUBLISHED
            }

            type Article @key(fields: "id") {
                id: ID!
                status: Status!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            enum Status {
                DRAFT
                PUBLISHED
                ARCHIVED
            }

            type Video @key(fields: "id") {
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
    
    // All enum values should be present
    assert!(schema_sdl.contains("DRAFT"));
    assert!(schema_sdl.contains("PUBLISHED"));
    assert!(schema_sdl.contains("ARCHIVED"));
    assert_snapshot!(schema_sdl);
}

#[test]
fn test_input_type_field_merging() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            input CreateUserInput {
                name: String!
                email: String!
            }

            type Query {
                createUser(input: CreateUserInput!): User
            }

            type User @key(fields: "id") {
                id: ID!
                name: String!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            input CreateUserInput {
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
fn test_complex_nested_type_merging() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            type User @key(fields: "id") {
                id: ID!
                profile: UserProfile!
                preferences: UserPreferences
            }

            type UserProfile {
                displayName: String!
                avatar: Image
            }

            type UserPreferences {
                theme: Theme!
                notifications: NotificationSettings!
            }

            type Image {
                url: String!
                alt: String
            }

            enum Theme {
                LIGHT
                DARK
            }

            type NotificationSettings {
                email: Boolean!
                push: Boolean!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            type User @key(fields: "id") {
                id: ID!
                profile: UserProfile!
                activity: UserActivity
            }

            type UserProfile {
                displayName: String!
                bio: String
            }

            type UserActivity {
                lastLogin: String
                loginCount: Int!
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