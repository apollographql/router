// this file is shared between the tests and benchmarks, using
// include!() instead of as a pub module, so it is only compiled
// in dev mode
use apollo_router::graphql::Response;
use apollo_router::plugin::test::MockSubgraph;
use apollo_router::services::router;
use apollo_router::services::supergraph;
use apollo_router::MockedSubgraphs;
use apollo_router::TestHarness;
use once_cell::sync::Lazy;
use serde_json::json;

use tower::{Service, ServiceExt};

static EXPECTED_RESPONSE: Lazy<Response> = Lazy::new(|| {
    serde_json::from_str(r#"{"data":{"topProducts":[{"upc":"1","name":"Table","reviews":[{"id":"1","product":{"name":"Table"},"author":{"id":"1","name":"Ada Lovelace"}},{"id":"4","product":{"name":"Table"},"author":{"id":"2","name":"Alan Turing"}}]},{"upc":"2","name":"Couch","reviews":[{"id":"2","product":{"name":"Couch"},"author":{"id":"1","name":"Ada Lovelace"}}]}]}}"#).unwrap()
});

static QUERY: &str = r#"query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }"#;

pub async fn basic_composition_benchmark(mut router_service: router::BoxCloneService) {
    let request = supergraph::Request::fake_builder()
        .query(QUERY.to_string())
        .variable("first", 2usize)
        .build()
        .expect("expecting valid request")
        .try_into()
        .unwrap();

    let response: Response = serde_json::from_slice(
        &router_service
            .ready()
            .await
            .unwrap()
            .call(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();

    assert_eq!(response, *EXPECTED_RESPONSE);
}

pub fn setup() -> TestHarness<'static> {
    let account_service =  MockSubgraph::builder().with_json(json!{{
        "query": "query TopProducts__accounts__3($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
        "operationName": "TopProducts__accounts__3",
        "variables": {
            "representations": [
                {
                    "__typename": "User",
                    "id": "1"
                },
                {
                    "__typename": "User",
                    "id": "2"
                }
            ]
        }
    }},
    json!{{
        "data": {
            "_entities": [
                {
                    "name": "Ada Lovelace"
                },
                {
                    "name": "Alan Turing"
                }
            ]
        }
    }}).build();

    let review_service = MockSubgraph::builder().with_json(json!{{
        "query": "query TopProducts__reviews__1($representations:[_Any!]!){_entities(representations:$representations){..._generated_onProduct1_0}}fragment _generated_onProduct1_0 on Product{reviews{id product{__typename upc}author{__typename id}}}",
        "operationName": "TopProducts__reviews__1",
        "variables": {
            "representations":[
                {
                    "__typename": "Product",
                    "upc":"1"
                },
                {
                    "__typename": "Product",
                    "upc": "2"
                }
            ]
        }
    }},
    json!{{
        "data": {
            "_entities": [
                {
                    "reviews": [
                        {
                            "id": "1",
                            "product": {
                                "__typename": "Product",
                                "upc": "1"
                            },
                            "author": {
                                "__typename": "User",
                                "id": "1"
                            }
                        },
                        {
                            "id": "4",
                            "product": {
                                "__typename": "Product",
                                "upc": "1"
                            },
                            "author": {
                                "__typename": "User",
                                "id": "2"
                            }
                        }
                    ]
                },
                {
                    "reviews": [
                        {
                            "id": "2",
                            "product": {
                                "__typename": "Product",
                                "upc": "2"
                            },
                            "author": {
                                "__typename": "User",
                                "id": "1"
                            }
                        }
                    ]
                }
            ]
        }
    }}).build();

    let product_service =  MockSubgraph::builder().with_json(json!{{
        "query": "query TopProducts__products__0($first:Int){topProducts(first:$first){__typename upc name}}",
        "operationName": "TopProducts__products__0",
        "variables":{
            "first":2u8
        },
    }},
    json!{{
        "data": {
            "topProducts": [
                {
                    "__typename": "Product",
                    "upc": "1",
                    "name":"Table"
                },
                {
                    "__typename": "Product",
                    "upc": "2",
                    "name": "Couch"
                }
            ]
        }
    }}).with_json(  json!{{
        "query": "query TopProducts__products__2($representations:[_Any!]!){_entities(representations:$representations){...on Product{name}}}",
        "operationName": "TopProducts__products__2",
        "variables": {
            "representations": [
                {
                    "__typename": "Product",
                    "upc": "1"
                },
                {
                    "__typename": "Product",
                    "upc": "2"
                }
            ]
        }
    }},
    json!{{
        "data": {
            "_entities": [
                {
                    "name": "Table"
                },
                {
                    "name": "Couch"
                }
            ]
        }
    }}).build();
    let mut mocks = MockedSubgraphs::default();
    mocks.insert("accounts", account_service);
    mocks.insert("reviews", review_service);
    mocks.insert("products", product_service);

    let schema = include_str!("../benches/fixtures/supergraph.graphql");
    TestHarness::builder()
        .try_log_level("info")
        .schema(schema)
        .extra_plugin(mocks)
}
