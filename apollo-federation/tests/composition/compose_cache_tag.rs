use apollo_federation::composition::compose;
use apollo_federation::subgraph::typestate::Subgraph;
use insta::assert_snapshot;
use test_log::test;

use crate::composition::ServiceDefinition;
use crate::composition::compose_as_fed2_subgraphs;

#[test]
fn composes_single_subgraph() {
    let products = ServiceDefinition {
        name: "products",
        type_defs: r#"
            type Query {
                product(id: ID!): Product @cacheTag(format: "product-{$args.id}")
                products: [Product!]! @cacheTag(format: "products")
            }

            type Product @key(fields: "id") @cacheTag(format: "product-{$key.id}") {
                id: ID!
                name: String!
            }
        "#,
    };
    let result = compose_as_fed2_subgraphs(&[products]).expect("composed successfully");
    assert_snapshot!(result.schema().schema());
}

#[test]
fn composes_with_multiple_subgraphs() {
    let products = ServiceDefinition {
        name: "products",
        type_defs: r#"
            type Query {
                product(id: ID!): Product @cacheTag(format: "product-{$args.id}")
                products: [Product!]! @cacheTag(format: "products")
            }

            type Product @key(fields: "id") @cacheTag(format: "product-{$key.id}") {
                id: ID!
                name: String!
            }
        "#,
    };
    let reviews = ServiceDefinition {
        name: "reviews",
        type_defs: r#"
          type Product @key(fields: "id") @cacheTag(format: "product-{$key.id}") {
            id: ID!
            reviews: [String!]!
          }
        "#,
    };
    let result = compose_as_fed2_subgraphs(&[products, reviews]).expect("composed successfully");
    assert_snapshot!(result.schema().schema());
}

#[test]
fn cache_tag_can_be_renamed() {
    let sdl = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", {name: "@cacheTag" as: "@myCacheTag"}])

        type Query {
            product(id: ID!): Product @myCacheTag(format: "product-{$args.id}")
        }

        type Product @key(fields: "id") @myCacheTag(format: "product-{$key.id}") {
            id: ID!
            name: String!
        }
    "#;
    let products =
        Subgraph::parse("products", "http://products/graphql", sdl).expect("parsed subgraph");
    let result = compose(vec![products]).expect("composed successfully");
    assert_snapshot!(result.schema().schema());
}
