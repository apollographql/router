//! End-to-end execution of GraphQL Federation lookup fetches.

use apollo_federation::composition::CompositionOptions;
use apollo_federation::subgraph::typestate::Subgraph;
use tower::ServiceExt;

use crate::TestHarness;
use crate::plugin::test::MockSubgraph;
use crate::services::supergraph;
use crate::test_harness::MockedSubgraphs;

fn compose(sources: &[(&str, &str)]) -> String {
    let subgraphs = sources
        .iter()
        .map(|(name, sdl)| Subgraph::parse(name, &format!("http://{name}"), sdl).expect("parses"))
        .collect();
    apollo_federation::composition::compose(subgraphs, CompositionOptions::default())
        .unwrap_or_else(|e| panic!("composition failed: {e:#?}"))
        .schema()
        .schema()
        .to_string()
}

const PRODUCTS: &str = r#"
    type Query {
      topProducts: [Product!]!
      productById(id: ID!): Product @lookup
    }
    type Product @key(fields: "id") {
      id: ID!
      name: String!
      dimension: Dimension!
    }
    type Dimension { size: Int! weight: Int! }
"#;

const REVIEWS: &str = r#"
    type Query {
      productById(id: ID!): Product @lookup @internal
    }
    type Product @key(fields: "id") {
      id: ID!
      reviewCount: Int!
    }
"#;

fn configuration() -> serde_json::Value {
    serde_json::json!({
        "include_subgraph_errors": { "all": true },
        "supergraph": { "query_planning": { "incremental_planner": { "enabled": true } } },
    })
}

async fn run(schema: &str, subgraphs: MockedSubgraphs, query: &str) -> crate::graphql::Response {
    let service = TestHarness::builder()
        .configuration_json(configuration())
        .unwrap()
        .schema(schema)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();
    let request = supergraph::Request::fake_builder()
        .query(query)
        .build()
        .unwrap();
    service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
}

#[tokio::test]
async fn resolves_entities_through_lookups() {
    let schema = compose(&[("products", PRODUCTS), ("reviews", REVIEWS)]);
    let subgraphs = MockedSubgraphs(
        [
            (
                "products",
                MockSubgraph::builder()
                    .with_json(
                        serde_json::json!({"query": "{ topProducts { __typename name id } }"}),
                        serde_json::json!({"data": {"topProducts": [
                            {"__typename": "Product", "name": "Table", "id": "1"},
                            {"__typename": "Product", "name": "Couch", "id": "2"},
                            {"__typename": "Product", "name": "Chair", "id": "1"},
                        ]}}),
                    )
                    .build(),
            ),
            (
                "reviews",
                MockSubgraph::builder()
                    .with_json(
                        serde_json::json!({
                            "query": "query($lookupArgument_0: ID!) { productById(id: $lookupArgument_0) { reviewCount } }",
                            "variables": {"lookupArgument_0": "1"},
                        }),
                        serde_json::json!({"data": {"productById": {"reviewCount": 10}}}),
                    )
                    .with_json(
                        serde_json::json!({
                            "query": "query($lookupArgument_0: ID!) { productById(id: $lookupArgument_0) { reviewCount } }",
                            "variables": {"lookupArgument_0": "2"},
                        }),
                        serde_json::json!({"data": {"productById": {"reviewCount": 20}}}),
                    )
                    .build(),
            ),
        ]
        .into_iter()
        .collect(),
    );
    let response = run(&schema, subgraphs, "{ topProducts { name reviewCount } }").await;
    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "topProducts": [
          {
            "name": "Table",
            "reviewCount": 10
          },
          {
            "name": "Couch",
            "reviewCount": 20
          },
          {
            "name": "Chair",
            "reviewCount": 10
          }
        ]
      }
    }
    "#);
}

const LOOKUP_QUERY: &str =
    "query($lookupArgument_0: ID!) { productById(id: $lookupArgument_0) { reviewCount } }";

fn products_mock() -> MockSubgraph {
    MockSubgraph::builder()
        .with_json(
            serde_json::json!({"query": "{ topProducts { __typename name id } }"}),
            serde_json::json!({"data": {"topProducts": [
                {"__typename": "Product", "name": "Table", "id": "1"},
                {"__typename": "Product", "name": "Couch", "id": "2"},
            ]}}),
        )
        .build()
}

#[tokio::test]
async fn maps_lookup_errors_and_nulls_to_entities() {
    let schema = compose(&[
        ("products", PRODUCTS),
        (
            "reviews",
            r#"
            type Query { productById(id: ID!): Product @lookup @internal }
            type Product @key(fields: "id") { id: ID! reviewCount: Int }
            "#,
        ),
    ]);
    let subgraphs = MockedSubgraphs(
        [
            ("products", products_mock()),
            (
                "reviews",
                MockSubgraph::builder()
                    // The entity does not exist: the lookup returns null.
                    .with_json(
                        serde_json::json!({"query": LOOKUP_QUERY, "variables": {"lookupArgument_0": "1"}}),
                        serde_json::json!({"data": {"productById": null}}),
                    )
                    // A field error under the lookup field belongs to the entity.
                    .with_json(
                        serde_json::json!({"query": LOOKUP_QUERY, "variables": {"lookupArgument_0": "2"}}),
                        serde_json::json!({
                            "data": {"productById": {"reviewCount": null}},
                            "errors": [{
                                "message": "reviews unavailable",
                                "path": ["productById", "reviewCount"],
                            }],
                        }),
                    )
                    .build(),
            ),
        ]
        .into_iter()
        .collect(),
    );
    let response = run(&schema, subgraphs, "{ topProducts { name reviewCount } }").await;
    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "topProducts": [
          {
            "name": "Table",
            "reviewCount": null
          },
          {
            "name": "Couch",
            "reviewCount": null
          }
        ]
      },
      "errors": [
        {
          "message": "reviews unavailable",
          "path": [
            "topProducts",
            1,
            "reviewCount"
          ],
          "extensions": {
            "service": "reviews"
          }
        }
      ]
    }
    "#);
}

#[tokio::test]
async fn passes_requirements_and_nested_lookups() {
    let schema = compose(&[
        ("products", PRODUCTS),
        (
            "shipping",
            r#"
            type Query { lookups: InternalLookups! @internal }
            type InternalLookups @internal {
              product(productId: ID! @is(field: "id")): Product @lookup
            }
            type Product @key(fields: "id") {
              id: ID!
              estimate(zip: String!, weight: Int! @require(field: "dimension.weight")): Int
            }
            "#,
        ),
    ]);
    let subgraphs = MockedSubgraphs(
        [
            (
                "products",
                MockSubgraph::builder()
                    .with_json(
                        serde_json::json!({"query": "{ topProducts { __typename name id dimension { weight } } }"}),
                        serde_json::json!({"data": {"topProducts": [
                            {"__typename": "Product", "name": "Table", "id": "1", "dimension": {"weight": 5}},
                        ]}}),
                    )
                    .build(),
            ),
            (
                "shipping",
                MockSubgraph::builder()
                    .with_json(
                        serde_json::json!({
                            "query": "query($lookupArgument_0: ID!, $requireArgument_0: Int!) { lookups { product(productId: $lookupArgument_0) { estimate(weight: $requireArgument_0, zip: \"94110\") } } }",
                            "variables": {"lookupArgument_0": "1", "requireArgument_0": 5},
                        }),
                        serde_json::json!({"data": {"lookups": {"product": {"estimate": 42}}}}),
                    )
                    .build(),
            ),
        ]
        .into_iter()
        .collect(),
    );
    let response = run(
        &schema,
        subgraphs,
        r#"{ topProducts { name estimate(zip: "94110") } }"#,
    )
    .await;
    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "topProducts": [
          {
            "name": "Table",
            "estimate": 42
          }
        ]
      }
    }
    "#);
}
