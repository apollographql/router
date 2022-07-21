// this file is shared between the tests and benchmarks, using
// include!() instead of as a pub module, so it is only compiled
// in dev mode
use apollo_router::plugin::test::MockSubgraph;
use apollo_router::services::{ RouterRequest, RouterResponse};
use apollo_router::services::PluggableRouterServiceBuilder;
use apollo_router::Schema;
use apollo_router::graphql::Response;
use once_cell::sync::Lazy;
use serde_json::json;
use std::sync::Arc;
use tower::{util::BoxCloneService, BoxError, Service, ServiceExt};

static EXPECTED_RESPONSE: Lazy<Response> = Lazy::new(|| {
    serde_json::from_str(r#"{"data":{"topProducts":[{"upc":"1","name":"Table","reviews":[{"id":"1","product":{"name":"Table"},"author":{"id":"1","name":"Ada Lovelace"}},{"id":"4","product":{"name":"Table"},"author":{"id":"2","name":"Alan Turing"}}]},{"upc":"2","name":"Couch","reviews":[{"id":"2","product":{"name":"Couch"},"author":{"id":"1","name":"Ada Lovelace"}}]}]}}"#).unwrap()
});

static QUERY: &str = r#"query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }"#;

pub async fn basic_composition_benchmark(
    mut router_service: BoxCloneService<RouterRequest, RouterResponse, BoxError>,
) {
    let request = RouterRequest::fake_builder()
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

pub fn setup() -> PluggableRouterServiceBuilder {
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

    let schema: Arc<Schema> = Arc::new(
        include_str!("../benches/fixtures/supergraph.graphql")
            .parse()
            .unwrap(),
    );

    let builder = PluggableRouterServiceBuilder::new(schema);

    builder
        .with_subgraph_service("accounts", account_service)
        .with_subgraph_service("reviews", review_service)
        .with_subgraph_service("products", product_service)
}
