use apollo_federation::merge::merge_federation_subgraphs;
use insta::assert_snapshot;

use crate::merger::{
    assert_composition_success, compose_as_fed2_subgraphs, extract_schemas, serialize_schema,
    test_subgraphs, basic_subgraph_template,
};

#[test]
fn test_tag_directive_propagation() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@tag"])

            type Query {
                _dummy: String
            }

            type User @key(fields: "id") @tag(name: "internal") {
                id: ID!
                name: String! @tag(name: "pii")
                email: String!
            }
        "#,
        "subgraph_b" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@tag"])

            type Query {
                _dummy: String
            }

            type User @key(fields: "id") {
                id: ID!
                profile: UserProfile! @tag(name: "profile")
            }

            type UserProfile @tag(name: "profile") {
                bio: String
                avatar: String @tag(name: "media")
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
fn test_inaccessible_directive_behavior() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@inaccessible"])

            type Query {
                _dummy: String
            }

            type User @key(fields: "id") {
                id: ID!
                name: String!
                internalId: String! @inaccessible
            }
        "#,
        "subgraph_b" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@inaccessible"])

            type Query {
                _dummy: String
            }

            type User @key(fields: "id") {
                id: ID!
                email: String!
                secretField: String! @inaccessible
            }

            type InternalType @inaccessible {
                data: String!
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
fn test_authenticated_directive() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])
                @link(url: "https://specs.apollo.dev/authenticated/v0.1", import: ["@authenticated"])

            type Query {
                _dummy: String
            }

            type User @key(fields: "id") {
                id: ID!
                name: String!
                profile: UserProfile! @authenticated
            }

            type UserProfile @authenticated {
                email: String!
                phone: String!
            }
        "#,
        "subgraph_b" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])
                @link(url: "https://specs.apollo.dev/authenticated/v0.1", import: ["@authenticated"])

            type Query {
                _dummy: String
            }

            type User @key(fields: "id") {
                id: ID!
                orders: [Order!]! @authenticated
            }

            type Order @key(fields: "id") @authenticated {
                id: ID!
                total: Float!
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
fn test_requires_scopes_directive() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])
                @link(url: "https://specs.apollo.dev/requiresScopes/v0.1", import: ["@requiresScopes"])

            type Query {
                _dummy: String
            }

            type User @key(fields: "id") {
                id: ID!
                name: String!
                adminData: String! @requiresScopes(scopes: [["admin"]])
            }
        "#,
        "subgraph_b" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])
                @link(url: "https://specs.apollo.dev/requiresScopes/v0.1", import: ["@requiresScopes"])

            type Query {
                _dummy: String
            }

            type User @key(fields: "id") {
                id: ID!
                sensitiveInfo: UserSensitiveInfo! @requiresScopes(scopes: [["user:read", "profile:read"]])
            }

            type UserSensitiveInfo @requiresScopes(scopes: [["user:read"]]) {
                ssn: String!
                creditScore: Int!
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
fn test_policy_directive() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])
                @link(url: "https://specs.apollo.dev/policy/v0.1", import: ["@policy"])

            type Query {
                _dummy: String
            }

            type User @key(fields: "id") {
                id: ID!
                name: String!
                personalData: String! @policy(policies: [["data-protection"]])
            }
        "#,
        "subgraph_b" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key"])
                @link(url: "https://specs.apollo.dev/policy/v0.1", import: ["@policy"])

            type Query {
                _dummy: String
            }

            type User @key(fields: "id") {
                id: ID!
                financialData: FinancialInfo! @policy(policies: [["financial-access"]])
            }

            type FinancialInfo @policy(policies: [["financial-read"]]) {
                balance: Float!
                transactions: [Transaction!]!
            }

            type Transaction {
                id: ID!
                amount: Float!
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
fn test_multiple_security_directives() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@tag"])
                @link(url: "https://specs.apollo.dev/authenticated/v0.1", import: ["@authenticated"])
                @link(url: "https://specs.apollo.dev/requiresScopes/v0.1", import: ["@requiresScopes"])

            type Query {
                _dummy: String
            }

            type User @key(fields: "id") @tag(name: "user") {
                id: ID!
                name: String!
                profile: UserProfile! @authenticated @tag(name: "profile")
            }

            type UserProfile @authenticated @requiresScopes(scopes: [["profile:read"]]) {
                email: String! @tag(name: "pii")
                preferences: String!
            }
        "#,
        "subgraph_b" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@tag"])
                @link(url: "https://specs.apollo.dev/policy/v0.1", import: ["@policy"])

            type Query {
                _dummy: String
            }

            type User @key(fields: "id") {
                id: ID!
                adminNotes: String! @policy(policies: [["admin-access"]]) @tag(name: "admin")
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
fn test_directive_composition_with_arguments() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@tag"])

            type Query {
                _dummy: String
            }

            type Product @key(fields: "id") @tag(name: "product") {
                id: ID!
                name: String! @tag(name: "public")
                price: Float! @tag(name: "pricing")
            }
        "#,
        "subgraph_b" => r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@tag"])

            type Query {
                _dummy: String
            }

            type Product @key(fields: "id") @tag(name: "inventory") {
                id: ID!
                stock: Int! @tag(name: "inventory")
                category: Category! @tag(name: "catalog")
            }

            type Category @tag(name: "catalog") {
                id: ID!
                name: String! @tag(name: "public")
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