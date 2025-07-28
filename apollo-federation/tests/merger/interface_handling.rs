use apollo_federation::merge::merge_federation_subgraphs;
use insta::assert_snapshot;

use crate::merger::{
    assert_composition_success, compose_as_fed2_subgraphs, extract_schemas, serialize_schema,
    test_subgraphs, basic_subgraph_template,
};

#[test]
fn test_interface_object_basic() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@interfaceObject"])

            type Query {
                _dummy: String
            }

            type Product @key(fields: "id") @interfaceObject {
                id: ID!
                name: String!
                price: Float!
            }
        "#,
        "subgraph_b" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

            type Query {
                _dummy: String
            }

            interface Product {
                id: ID!
                name: String!
            }

            type Book implements Product @key(fields: "id") {
                id: ID!
                name: String!
                author: String!
                isbn: String!
            }

            type Movie implements Product @key(fields: "id") {
                id: ID!
                name: String!
                director: String!
                duration: Int!
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
fn test_interface_object_with_implementations() {
    let subgraphs = test_subgraphs! {
        "catalog" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@interfaceObject"])

            type Query {
                _dummy: String
            }

            type Product @key(fields: "id") @interfaceObject {
                id: ID!
                name: String!
                description: String!
            }
        "#,
        "books" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

            type Query {
                _dummy: String
            }

            interface Product {
                id: ID!
                name: String!
            }

            type Book implements Product @key(fields: "id") {
                id: ID!
                name: String!
                author: String!
                pages: Int!
            }
        "#,
        "electronics" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])

            type Query {
                _dummy: String
            }

            interface Product {
                id: ID!
                name: String!
            }

            type Laptop implements Product @key(fields: "id") {
                id: ID!
                name: String!
                brand: String!
                specs: String!
            }

            type Phone implements Product @key(fields: "id") {
                id: ID!
                name: String!
                brand: String!
                screenSize: Float!
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
fn test_interface_field_merging() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            interface Node {
                id: ID!
                createdAt: String!
            }

            type User implements Node @key(fields: "id") {
                id: ID!
                createdAt: String!
                name: String!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            interface Node {
                id: ID!
                createdAt: String!
                updatedAt: String!
            }

            type User implements Node @key(fields: "id") {
                id: ID!
                createdAt: String!
                updatedAt: String!
                email: String!
            }

            type Product implements Node @key(fields: "id") {
                id: ID!
                createdAt: String!
                updatedAt: String!
                name: String!
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
fn test_interface_implementing_interface() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            interface Node {
                id: ID!
            }

            interface Timestamped implements Node {
                id: ID!
                createdAt: String!
            }

            type User implements Node & Timestamped @key(fields: "id") {
                id: ID!
                createdAt: String!
                name: String!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            interface Node {
                id: ID!
            }

            interface Timestamped implements Node {
                id: ID!
                createdAt: String!
                updatedAt: String!
            }

            type User implements Node & Timestamped @key(fields: "id") {
                id: ID!
                createdAt: String!
                updatedAt: String!
                email: String!
            }

            type Article implements Node & Timestamped @key(fields: "id") {
                id: ID!
                createdAt: String!
                updatedAt: String!
                title: String!
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
fn test_interface_with_federation_directives() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            interface Content @key(fields: "id") {
                id: ID!
                title: String!
                author: User!
            }

            type User @key(fields: "id") {
                id: ID!
                name: String!
            }

            type Article implements Content @key(fields: "id") {
                id: ID!
                title: String!
                author: User!
                body: String!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            interface Content @key(fields: "id") {
                id: ID!
                title: String!
                publishedAt: String! @external
                isPublished: Boolean! @requires(fields: "publishedAt")
            }

            type User @key(fields: "id") {
                id: ID!
                email: String!
            }

            type Video implements Content @key(fields: "id") {
                id: ID!
                title: String!
                publishedAt: String!
                isPublished: Boolean!
                duration: Int!
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
fn test_complex_interface_hierarchy() {
    let subgraphs = test_subgraphs! {
        "core" => basic_subgraph_template("core", r#"
            interface Node {
                id: ID!
            }

            interface Timestamped implements Node {
                id: ID!
                createdAt: String!
                updatedAt: String!
            }

            interface Owned implements Node & Timestamped {
                id: ID!
                createdAt: String!
                updatedAt: String!
                owner: User!
            }

            type User implements Node & Timestamped @key(fields: "id") {
                id: ID!
                createdAt: String!
                updatedAt: String!
                username: String!
            }
        "#),
        "content" => basic_subgraph_template("content", r#"
            interface Node {
                id: ID!
            }

            interface Timestamped implements Node {
                id: ID!
                createdAt: String!
                updatedAt: String!
            }

            interface Owned implements Node & Timestamped {
                id: ID!
                createdAt: String!
                updatedAt: String!
                owner: User!
            }

            type User implements Node & Timestamped @key(fields: "id") {
                id: ID!
                createdAt: String!
                updatedAt: String!
                email: String!
            }

            type Article implements Node & Timestamped & Owned @key(fields: "id") {
                id: ID!
                createdAt: String!
                updatedAt: String!
                owner: User!
                title: String!
                content: String!
            }

            type Comment implements Node & Timestamped & Owned @key(fields: "id") {
                id: ID!
                createdAt: String!
                updatedAt: String!
                owner: User!
                text: String!
                article: Article!
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