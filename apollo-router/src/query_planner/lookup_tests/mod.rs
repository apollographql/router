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
        "preview_graphql_federation": { "enabled": true },
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

/// The authorization metadata of a lookup fetch (part of the subgraph cache key, e.g. for response
/// caching) must reflect the requirements of the entity fields it selects, as for `_entities`.
#[test]
fn lookup_fetch_authorization_metadata_covers_entity_fields() {
    use apollo_federation::query_plan::PlanNode as NextPlanNode;
    use apollo_federation::query_plan::TopLevelPlanNode;

    use crate::plugins::authorization::CacheKeyMetadata;
    use crate::query_planner::PlanNode;

    let schema = compose(&[
        ("products", PRODUCTS),
        (
            "reviews",
            r#"
            type Query { productById(id: ID!): Product @lookup @internal }
            type Product @key(fields: "id") {
              id: ID!
              reviewCount: Int! @authenticated @requiresScopes(scopes: [["read:reviews"]])
            }
            "#,
        ),
    ]);
    let supergraph = apollo_federation::Supergraph::new_with_router_specs(&schema).unwrap();
    let mut config = apollo_federation::query_plan::query_planner::QueryPlannerConfig::default();
    config.incremental_planner.enabled = true;
    let planner =
        apollo_federation::query_plan::query_planner::QueryPlanner::new(&supergraph, config)
            .unwrap();
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        "{ topProducts { reviewCount } }",
        "op.graphql",
    )
    .unwrap();
    let plan = planner
        .build_query_plan(&document, None, Default::default())
        .unwrap();
    let Some(TopLevelPlanNode::Sequence(sequence)) = &plan.node else {
        panic!("unexpected plan: {plan}")
    };
    let NextPlanNode::Flatten(flatten) = &sequence.nodes[1] else {
        panic!("unexpected plan: {plan}")
    };
    let NextPlanNode::Fetch(fetch) = &*flatten.node else {
        panic!("unexpected plan: {plan}")
    };
    assert!(fetch.entity_lookup.is_some(), "{plan}");
    let PlanNode::Fetch(mut fetch) = PlanNode::from(fetch) else {
        unreachable!()
    };

    let router_schema = crate::spec::Schema::parse(&schema, &Default::default()).unwrap();
    let client_key = CacheKeyMetadata {
        is_authenticated: true,
        scopes: vec!["read:reviews".to_string()],
        policies: vec![],
    };
    fetch.extract_authorization_metadata(router_schema.supergraph_schema(), &client_key);
    assert!(
        fetch.authorization.is_authenticated,
        "{:?}",
        fetch.authorization
    );
    assert_eq!(fetch.authorization.scopes, ["read:reviews"]);
}

/// The same holds for `_entities` fetches: `_entities` is not part of the supergraph, so the
/// metadata must come from the entity selections.
#[test]
fn entities_fetch_authorization_metadata_covers_entity_fields() {
    use apollo_federation::query_plan::PlanNode as NextPlanNode;
    use apollo_federation::query_plan::TopLevelPlanNode;

    use crate::plugins::authorization::CacheKeyMetadata;
    use crate::query_planner::PlanNode;

    let link = r#"extend schema @link(url: "https://specs.apollo.dev/federation/v2.9", import: ["@key", "@authenticated", "@requiresScopes"])"#;
    let schema = compose(&[
        (
            "products",
            &format!(
                "{link}\ntype Query {{ topProducts: [Product!]! }} type Product @key(fields: \"id\") {{ id: ID! name: String! }}"
            ),
        ),
        (
            "reviews",
            &format!(
                "{link}\ntype Product @key(fields: \"id\") {{ id: ID! reviewCount: Int! @authenticated @requiresScopes(scopes: [[\"read:reviews\"]]) }}"
            ),
        ),
    ]);
    let supergraph = apollo_federation::Supergraph::new_with_router_specs(&schema).unwrap();
    let planner = apollo_federation::query_plan::query_planner::QueryPlanner::new(
        &supergraph,
        Default::default(),
    )
    .unwrap();
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        "{ topProducts { reviewCount } }",
        "op.graphql",
    )
    .unwrap();
    let plan = planner
        .build_query_plan(&document, None, Default::default())
        .unwrap();
    let Some(TopLevelPlanNode::Sequence(sequence)) = &plan.node else {
        panic!("unexpected plan: {plan}")
    };
    let NextPlanNode::Flatten(flatten) = &sequence.nodes[1] else {
        panic!("unexpected plan: {plan}")
    };
    let NextPlanNode::Fetch(fetch) = &*flatten.node else {
        panic!("unexpected plan: {plan}")
    };
    let PlanNode::Fetch(mut fetch) = PlanNode::from(fetch) else {
        unreachable!()
    };
    let router_schema = crate::spec::Schema::parse(&schema, &Default::default()).unwrap();
    let client_key = CacheKeyMetadata {
        is_authenticated: true,
        scopes: vec!["read:reviews".to_string()],
        policies: vec![],
    };
    fetch.extract_authorization_metadata(router_schema.supergraph_schema(), &client_key);
    assert!(
        fetch.authorization.is_authenticated,
        "{:?}",
        fetch.authorization
    );
    assert_eq!(fetch.authorization.scopes, ["read:reviews"]);
}

/// Lookup requests to a subgraph configured for variable batching reach it as one HTTP request
/// with a list of variable sets, and its `application/jsonl` answer is placed by `variableIndex`.
#[tokio::test(flavor = "multi_thread")]
async fn variable_batched_lookups_over_http() {
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers;

    let schema = compose(&[("products", PRODUCTS), ("reviews", REVIEWS)]);
    let server = MockServer::start().await;
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/products"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {"topProducts": [
                {"__typename": "Product", "name": "Table", "id": "1"},
                {"__typename": "Product", "name": "Couch", "id": "2"},
            ]}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/reviews"))
        .and(matchers::body_json(serde_json::json!({
            "query": "query($lookupArgument_0: ID!) { productById(id: $lookupArgument_0) { reviewCount } }",
            "variables": [{"lookupArgument_0": "1"}, {"lookupArgument_0": "2"}],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            [
                r#"{"variableIndex":1,"data":{"productById":{"reviewCount":20}}}"#,
                r#"{"variableIndex":0,"data":{"productById":{"reviewCount":10}}}"#,
            ]
            .join("\n"),
            "application/jsonl",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let mut config = configuration();
    config["override_subgraph_url"] = serde_json::json!({
        "products": format!("{}/products", server.uri()),
        "reviews": format!("{}/reviews", server.uri()),
    });
    config["preview_graphql_federation"]["subgraph"] =
        serde_json::json!({"all": {"variable_batching": true}});
    let service = TestHarness::builder()
        .configuration_json(config)
        .unwrap()
        .schema(&schema)
        .with_subgraph_network_requests()
        .build_supergraph()
        .await
        .unwrap();
    let request = supergraph::Request::fake_builder()
        .query("{ topProducts { name reviewCount } }")
        .build()
        .unwrap();
    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();
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
          }
        ]
      }
    }
    "#);
}
