use apollo_federation::merge::merge_federation_subgraphs;
use insta::assert_snapshot;

use crate::merger::{
    assert_composition_success, compose_as_fed2_subgraphs, extract_schemas, serialize_schema,
    test_subgraphs, basic_subgraph_template,
};

#[test]
fn test_enum_type_merging_comprehensive() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            enum Status {
                DRAFT
                PUBLISHED
                ARCHIVED
            }

            enum Priority {
                LOW
                MEDIUM
                HIGH
            }

            type Article @key(fields: "id") {
                id: ID!
                status: Status!
                priority: Priority!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            enum Status {
                DRAFT
                PUBLISHED
                ARCHIVED
                DELETED
            }

            enum Priority {
                LOW
                MEDIUM
                HIGH
                URGENT
            }

            type Task @key(fields: "id") {
                id: ID!
                status: Status!
                priority: Priority!
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
    assert!(schema_sdl.contains("DELETED"));
    assert!(schema_sdl.contains("URGENT"));
    assert_snapshot!(schema_sdl);
}

#[test]
fn test_input_type_comprehensive_merging() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            input CreateUserInput {
                name: String!
                email: String!
                age: Int
                preferences: UserPreferencesInput
            }

            input UserPreferencesInput {
                theme: String!
                notifications: Boolean!
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
                preferences: UserPreferencesInput
                profile: UserProfileInput
            }

            input UserPreferencesInput {
                theme: String!
                notifications: Boolean!
                language: String
            }

            input UserProfileInput {
                bio: String
                avatar: String
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
fn test_union_type_comprehensive_merging() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            union SearchResult = User | Article

            union Content = Article | Video

            type User @key(fields: "id") {
                id: ID!
                name: String!
            }

            type Article @key(fields: "id") {
                id: ID!
                title: String!
                content: String!
            }

            type Video @key(fields: "id") {
                id: ID!
                title: String!
                duration: Int!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            union SearchResult = User | Product

            union Content = Article | Image

            type User @key(fields: "id") {
                id: ID!
                email: String!
            }

            type Product @key(fields: "id") {
                id: ID!
                name: String!
                price: Float!
            }

            type Article @key(fields: "id") {
                id: ID!
                author: String!
            }

            type Image @key(fields: "id") {
                id: ID!
                url: String!
                alt: String
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    assert_composition_success!(result);

    let success = result.unwrap();
    let (schema, _hints) = extract_schemas(&success);
    let schema_sdl = serialize_schema(schema);
    
    // All union members should be present
    assert!(schema_sdl.contains("User"));
    assert!(schema_sdl.contains("Article"));
    assert!(schema_sdl.contains("Product"));
    assert!(schema_sdl.contains("Video"));
    assert!(schema_sdl.contains("Image"));
    assert_snapshot!(schema_sdl);
}

#[test]
fn test_scalar_type_merging() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            scalar DateTime
            scalar JSON
            scalar Upload

            type User @key(fields: "id") {
                id: ID!
                createdAt: DateTime!
                metadata: JSON
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            scalar DateTime
            scalar JSON
            scalar URL

            type Product @key(fields: "id") {
                id: ID!
                updatedAt: DateTime!
                config: JSON
                website: URL
            }
        "#),
    };

    let result = compose_as_fed2_subgraphs(subgraphs);
    assert_composition_success!(result);

    let success = result.unwrap();
    let (schema, _hints) = extract_schemas(&success);
    let schema_sdl = serialize_schema(schema);
    
    // All custom scalars should be present
    assert!(schema_sdl.contains("scalar DateTime"));
    assert!(schema_sdl.contains("scalar JSON"));
    assert!(schema_sdl.contains("scalar Upload"));
    assert!(schema_sdl.contains("scalar URL"));
    assert_snapshot!(schema_sdl);
}

#[test]
fn test_complex_nested_input_types() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            input CreateOrderInput {
                items: [OrderItemInput!]!
                shipping: ShippingInput!
                payment: PaymentInput!
            }

            input OrderItemInput {
                productId: ID!
                quantity: Int!
                customizations: [CustomizationInput!]
            }

            input ShippingInput {
                address: AddressInput!
                method: ShippingMethod!
            }

            input PaymentInput {
                method: PaymentMethod!
                cardToken: String
            }

            input AddressInput {
                street: String!
                city: String!
                country: String!
                postalCode: String!
            }

            input CustomizationInput {
                type: String!
                value: String!
            }

            enum ShippingMethod {
                STANDARD
                EXPRESS
                OVERNIGHT
            }

            enum PaymentMethod {
                CREDIT_CARD
                PAYPAL
                BANK_TRANSFER
            }

            type Query {
                createOrder(input: CreateOrderInput!): Order
            }

            type Order @key(fields: "id") {
                id: ID!
                total: Float!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            input CreateOrderInput {
                items: [OrderItemInput!]!
                shipping: ShippingInput!
                payment: PaymentInput!
                coupon: CouponInput
            }

            input OrderItemInput {
                productId: ID!
                quantity: Int!
                customizations: [CustomizationInput!]
                giftWrap: Boolean
            }

            input ShippingInput {
                address: AddressInput!
                method: ShippingMethod!
                instructions: String
            }

            input PaymentInput {
                method: PaymentMethod!
                cardToken: String
                billingAddress: AddressInput
            }

            input AddressInput {
                street: String!
                city: String!
                country: String!
                postalCode: String!
                apartment: String
            }

            input CustomizationInput {
                type: String!
                value: String!
                price: Float
            }

            input CouponInput {
                code: String!
                type: CouponType!
            }

            enum ShippingMethod {
                STANDARD
                EXPRESS
                OVERNIGHT
                PICKUP
            }

            enum PaymentMethod {
                CREDIT_CARD
                PAYPAL
                BANK_TRANSFER
                CRYPTO
            }

            enum CouponType {
                PERCENTAGE
                FIXED_AMOUNT
                FREE_SHIPPING
            }

            type Order @key(fields: "id") {
                id: ID!
                status: String!
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
fn test_enum_with_descriptions_and_deprecation() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            """
            Status of a content item
            """
            enum ContentStatus {
                """
                Content is being drafted
                """
                DRAFT
                
                """
                Content is published and visible
                """
                PUBLISHED
                
                """
                Content is archived but not deleted
                """
                ARCHIVED
            }

            type Article @key(fields: "id") {
                id: ID!
                status: ContentStatus!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            """
            Status of a content item
            """
            enum ContentStatus {
                DRAFT
                PUBLISHED
                ARCHIVED
                
                """
                Content is permanently deleted
                """
                DELETED
            }

            type Video @key(fields: "id") {
                id: ID!
                status: ContentStatus!
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
fn test_union_with_interface_members() {
    let subgraphs = test_subgraphs! {
        "subgraph_a" => basic_subgraph_template("subgraph_a", r#"
            interface Content {
                id: ID!
                title: String!
            }

            union SearchResult = Article | Video

            type Article implements Content @key(fields: "id") {
                id: ID!
                title: String!
                body: String!
            }

            type Video implements Content @key(fields: "id") {
                id: ID!
                title: String!
                duration: Int!
            }
        "#),
        "subgraph_b" => basic_subgraph_template("subgraph_b", r#"
            interface Content {
                id: ID!
                title: String!
                author: String!
            }

            union SearchResult = Article | Podcast

            type Article implements Content @key(fields: "id") {
                id: ID!
                title: String!
                author: String!
                tags: [String!]!
            }

            type Podcast implements Content @key(fields: "id") {
                id: ID!
                title: String!
                author: String!
                episodes: Int!
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