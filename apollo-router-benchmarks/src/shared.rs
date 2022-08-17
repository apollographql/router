// this file is shared between the tests and benchmarks, using
// include!() instead of as a pub module, so it is only compiled
// in dev mode
use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInit;
use apollo_router::plugin::test::MockSubgraph;
use apollo_router::stages::router;
use apollo_router::stages::subgraph;
use apollo_router::TestHarness;
use apollo_router::graphql::Response;
use once_cell::sync::Lazy;
use serde_json::json;
use std::collections::HashMap;

use tower::{BoxError, Service, ServiceExt};

static EXPECTED_RESPONSE: Lazy<Response> = Lazy::new(|| {
    serde_json::from_str(r#"{"data":{"topProducts":[{"upc":"1","name":"Table","reviews":[{"id":"1","product":{"name":"Table"},"author":{"id":"1","name":"Ada Lovelace"}},{"id":"4","product":{"name":"Table"},"author":{"id":"2","name":"Alan Turing"}}]},{"upc":"2","name":"Couch","reviews":[{"id":"2","product":{"name":"Couch"},"author":{"id":"1","name":"Ada Lovelace"}}]}]}}"#).unwrap()
});

static QUERY: &str = r#"query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }"#;

pub async fn basic_composition_benchmark(
    mut router_service: router::BoxCloneService,
) {
    let request = router::Request::fake_builder()
        .query(QUERY.to_string())
        .variable("first", 2usize)
        .build().expect("expecting valid request");

    let response = router_service
        .ready()
        .await
        .unwrap()
        .call(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    assert_eq!(response, *EXPECTED_RESPONSE);
}

pub fn setup() -> TestHarness<'static> {
    let account_mocks = vec![
            (
                json!{{
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
                            },
                            {
                                "__typename": "User",
                                "id": "1"
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
                            },
                            {
                                "name": "Ada Lovelace"
                            }
                        ]
                    }
                }}
            )
        ].into_iter().map(|(query, response)| (serde_json::from_value(query).unwrap(), serde_json::from_value(response).unwrap())).collect();
    let account_service = MockSubgraph::new(account_mocks);

    let review_mocks = vec![
            (
                json!{{
                    "query": "query TopProducts__reviews__1($representations:[_Any!]!){_entities(representations:$representations){...on Product{reviews{id product{__typename upc}author{__typename id}}}}}",
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
                }}
            )
            ].into_iter().map(|(query, response)| (serde_json::from_value(query).unwrap(), serde_json::from_value(response).unwrap())).collect();
    let review_service = MockSubgraph::new(review_mocks);


    let product_mocks = vec![
            (
                json!{{
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
                }}
            ),
            (
                json!{{
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
                                "name": "Table"
                            },
                            {
                                "name": "Couch"
                            }
                        ]
                    }
                }}
            )
            ].into_iter().map(|(query, response)| (serde_json::from_value(query).unwrap(), serde_json::from_value(response).unwrap())).collect();
    let product_service = MockSubgraph::new(product_mocks);

    let mut mocks = HashMap::new();
    mocks.insert("accounts", account_service);
    mocks.insert("reviews", review_service);
    mocks.insert("products", product_service);

    let schema = include_str!("../benches/fixtures/supergraph.graphql");
    TestHarness::builder().schema(schema).extra_plugin(MockedSubgraphs(mocks))
}

struct MockedSubgraphs(HashMap<&'static str, MockSubgraph>);

#[async_trait::async_trait]
impl Plugin for MockedSubgraphs {
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!()
    }

    fn subgraph_service(
        &self,
        subgraph_name: &str,
        default: subgraph::BoxService,
    ) -> subgraph::BoxService {
        self.0
            .get(subgraph_name)
            .map(|service| service.clone().boxed())
            .unwrap_or(default)
    }
}