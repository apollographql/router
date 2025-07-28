use apollo_federation::merge::merge_federation_subgraphs;
use insta::assert_snapshot;

use crate::merger::{
    assert_composition_success, assert_composition_failure, compose_as_fed2_subgraphs,
    extract_schemas, serialize_schema, test_subgraphs, basic_subgraph_template,
};

#[test]
fn test_key_directive_basic() {
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
            }

            type Post @key(fields: "id") {
                id: ID!
                title: String!
                author: User!
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
fn test_key_directive_multiple_keys() {
    let subgraphs = test_subgraphs! {
        "users" => basic_subgraph_template("users", r#"
            type User @key(fields: "id") @key(fields: "email") {
                id: ID!
                email: String!
                name: String!
            }
        "#),
        "profiles" => basic_subgraph_template("profiles", r#"
            type User @key(fields: "email") {
                email: String!
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
fn test_key_directive_compound_keys() {
    let subgraphs = test_subgraphs! {
        "inventory" => basic_subgraph_template("inventory", r#"
            type Product @key(fields: "sku warehouse") {
                sku: String!
                warehouse: String!
                quantity: Int!
            }
        "#),
        "catalog" => basic_subgraph_template("catalog", r#"
            type Product @key(fields: "sku warehouse") {
                sku: String!
                warehouse: String!
                name: String!
                price: Float!
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
fn test_external_directive() {
    let subgraphs = test_subgraphs! {
        "users" => basic_subgraph_template("users", r#"
            type User @key(fields: "id") {
                id: ID!
                name: String!
                email: String!
            }
        "#),
        "posts" => basic_subgraph_template("posts", r#"
            type User @key(fields: "id") {
                id: ID!
                name: String! @external
            }

            type Post @key(fields: "id") {
                id: ID!
                title: String!
                author: User!
                authorName: String! @requires(fields: "author { name }")
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
fn test_requires_directive() {
    let subgraphs = test_subgraphs! {
        "users" => basic_subgraph_template("users", r#"
            type User @key(fields: "id") {
                id: ID!
                firstName: String!
                lastName: String!
            }
        "#),
        "profiles" => basic_subgraph_template("profiles", r#"
            type User @key(fields: "id") {
                id: ID!
                firstName: String! @external
                lastName: String! @external
                fullName: String! @requires(fields: "firstName lastName")
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
fn test_provides_directive() {
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
                name: String! @external
            }

            type Post @key(fields: "id") {
                id: ID!
                title: String!
                author: User! @provides(fields: "name")
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
fn test_shareable_directive() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@shareable"])

            type Query {
                _dummy: String
            }

            type Product @key(fields: "id") {
                id: ID!
                name: String! @shareable
                price: Float!
            }
        "#,
        "subgraph_b" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@shareable"])

            type Query {
                _dummy: String
            }

            type Product @key(fields: "id") {
                id: ID!
                name: String! @shareable
                description: String!
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    assert_composition_success!(result);

    let success = result.unwrap();
    let (schema, _hints) = extract_schemas(&success);
    let schema_sdl = serialize_schema(schema);
    
    assert_snapshot!(schema_sdl);
}

#[test]
fn test_override_directive() {
    let subgraphs = test_subgraphs! {
        "legacy" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])

            type Query {
                _dummy: String
            }

            type Product @key(fields: "id") {
                id: ID!
                name: String!
                price: Float!
            }
        "#,
        "new" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])

            type Query {
                _dummy: String
            }

            type Product @key(fields: "id") {
                id: ID!
                name: String! @override(from: "legacy")
                description: String!
            }
        "#,
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    assert_composition_success!(result);

    let success = result.unwrap();
    let (schema, _hints) = extract_schemas(&success);
    let schema_sdl = serialize_schema(schema);
    
    assert_snapshot!(schema_sdl);
}

#[test]
fn test_complex_federation_scenario() {
    let subgraphs = test_subgraphs! {
        "accounts" => basic_subgraph_template("accounts", r#"
            type User @key(fields: "id") {
                id: ID!
                username: String!
                email: String!
            }
        "#),
        "products" => basic_subgraph_template("products", r#"
            type Product @key(fields: "upc") {
                upc: String!
                name: String!
                price: Float!
            }
        "#),
        "reviews" => basic_subgraph_template("reviews", r#"
            type User @key(fields: "id") {
                id: ID!
                username: String! @external
                reviews: [Review!]!
            }

            type Product @key(fields: "upc") {
                upc: String!
                name: String! @external
                reviews: [Review!]!
            }

            type Review @key(fields: "id") {
                id: ID!
                body: String!
                rating: Int!
                author: User! @provides(fields: "username")
                product: Product! @provides(fields: "name")
            }
        "#),
        "inventory" => basic_subgraph_template("inventory", r#"
            type Product @key(fields: "upc") {
                upc: String!
                weight: Float! @external
                price: Float! @external
                inStock: Boolean!
                shippingEstimate: String! @requires(fields: "price weight")
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
fn test_nested_key_fields() {
    let subgraphs = test_subgraphs! {
        "users" => basic_subgraph_template("users", r#"
            type User @key(fields: "profile { id }") {
                profile: UserProfile!
                name: String!
            }

            type UserProfile {
                id: ID!
                displayName: String!
            }
        "#),
        "posts" => basic_subgraph_template("posts", r#"
            type User @key(fields: "profile { id }") {
                profile: UserProfile!
                posts: [Post!]!
            }

            type UserProfile {
                id: ID!
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