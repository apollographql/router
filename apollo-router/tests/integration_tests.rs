use apollo_router::configuration::Configuration;
use apollo_router::get_dispatcher;
use apollo_router::reqwest_subgraph_service::ReqwestSubgraphService;
use apollo_router_core::prelude::*;
use apollo_router_core::{
    Context, PluggableRouterServiceBuilder, ResponseBody, SubgraphRequest, ValueExt,
};
use maplit::hashmap;
use serde_json::to_string_pretty;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use test_span::prelude::*;
use tower::Service;
use tower::ServiceExt;

macro_rules! assert_federated_response {
    ($query:expr, $service_requests:expr $(,)?) => {
        let request = graphql::Request::builder()
            .query($query.to_string())
            .variables(Arc::new(
                vec![
                    ("topProductsFirst".into(), 2.into()),
                    ("reviewsForAuthorAuthorId".into(), 1.into()),
                ]
                .into_iter()
                .collect(),
            ))
            .build();



        let expected = query_node(&request).await.unwrap();

        let http_request = http::Request::builder()
            .method("POST")
            .body(request)
            .unwrap().into();

        let request = graphql::RouterRequest {
            context: Context::new().with_request(http_request),
        };

        let (actual, registry) = query_rust(request).await;


        tracing::debug!("query:\n{}\n", $query);

        assert!(
            expected.data.is_object(),
            "nodejs: no response's data: please check that the gateway and the subgraphs are running",
        );

        tracing::debug!("expected: {}", to_string_pretty(&expected).unwrap());
        tracing::debug!("actual: {}", to_string_pretty(&actual).unwrap());

        assert!(expected.data.eq_and_ordered(&actual.data));
        assert_eq!(registry.totals(), $service_requests);
    };
}

#[tokio::test]
async fn basic_request() {
    assert_federated_response!(
        r#"{ topProducts { name name2:name } }"#,
        hashmap! {
            "products".to_string()=>1,
        },
    );
}

#[tokio::test]
async fn basic_composition() {
    assert_federated_response!(
        r#"{ topProducts { upc name reviews {id product { name } author { id name } } } }"#,
        hashmap! {
            "products".to_string()=>2,
            "reviews".to_string()=>1,
            "accounts".to_string()=>1,
        },
    );
}

#[test_span(tokio::test)]
#[target(apollo_router=tracing::Level::DEBUG)]
#[target(apollo_router_core=tracing::Level::DEBUG)]
async fn traced_basic_request() {
    assert_federated_response!(
        r#"{ topProducts { name name2:name } }"#,
        hashmap! {
            "products".to_string()=>1,
        },
    );
    insta::assert_json_snapshot!("traced_basic_request", get_spans());
}

#[test_span(tokio::test)]
#[target(apollo_router=tracing::Level::DEBUG)]
#[target(apollo_router_core=tracing::Level::DEBUG)]
async fn traced_basic_composition() {
    assert_federated_response!(
        r#"{ topProducts { upc name reviews {id product { name } author { id name } } } }"#,
        hashmap! {
            "products".to_string()=>2,
            "reviews".to_string()=>1,
            "accounts".to_string()=>1,
        },
    );
    insta::assert_json_snapshot!("traced_basic_composition", get_spans());
}

#[tokio::test]
async fn basic_mutation() {
    assert_federated_response!(
        r#"mutation {
              createProduct(upc:"8", name:"Bob") {
                upc
                name
                reviews {
                  body
                }
              }
              createReview(upc: "8", id:"100", body: "Bif"){
                id
                body
              }
            }"#,
        hashmap! {
            "products".to_string()=>1,
            "reviews".to_string()=>2,
        },
    );
}

#[tokio::test]
async fn queries_should_work_over_get() {
    let request = graphql::Request::builder()
        .query(r#"{ topProducts { upc name reviews {id product { name } author { id name } } } }"#)
        .variables(Arc::new(
            vec![
                ("topProductsFirst".into(), 2.into()),
                ("reviewsForAuthorAuthorId".into(), 1.into()),
            ]
            .into_iter()
            .collect(),
        ))
        .build();

    let expected_service_hits = hashmap! {
        "products".to_string()=>2,
        "reviews".to_string()=>1,
        "accounts".to_string()=>1,
    };

    let http_request = http::Request::builder()
        .method("GET")
        .body(request)
        .unwrap()
        .into();

    let request = graphql::RouterRequest {
        context: graphql::Context::new().with_request(http_request),
    };

    let (actual, registry) = query_rust(request).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test]
async fn service_errors_should_be_propagated() {
    let expected_error =apollo_router_core::Error {
        message :"Value retrieval failed: Query planning had errors: Planning errors: UNKNOWN: Unknown operation named \"invalidOperationName\"".to_string(),
        ..Default::default()
    };

    let request = graphql::Request::builder()
        .query(r#"{ topProducts { name } }"#)
        .operation_name(Some("invalidOperationName".to_string()))
        .build();

    let expected_service_hits = hashmap! {};

    let http_request = http::Request::builder()
        .method("GET")
        .body(request)
        .unwrap()
        .into();

    let request = graphql::RouterRequest {
        context: graphql::Context::new().with_request(http_request),
    };

    let (actual, registry) = query_rust(request).await;

    assert_eq!(expected_error, actual.errors[0]);
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test]
async fn mutation_should_not_work_over_get() {
    let request = graphql::Request::builder()
        .query(
            r#"mutation {
                createProduct(upc:"8", name:"Bob") {
                  upc
                  name
                  reviews {
                    body
                  }
                }
                createReview(upc: "8", id:"100", body: "Bif"){
                  id
                  body
                }
              }"#,
        )
        .variables(Arc::new(
            vec![
                ("topProductsFirst".into(), 2.into()),
                ("reviewsForAuthorAuthorId".into(), 1.into()),
            ]
            .into_iter()
            .collect(),
        ))
        .build();

    // No services should be queried
    let expected_service_hits = hashmap! {};

    let http_request = http::Request::builder()
        .method("GET")
        .body(request)
        .unwrap()
        .into();

    let request = graphql::RouterRequest {
        context: graphql::Context::new().with_request(http_request),
    };

    let (actual, registry) = query_rust(request).await;

    assert_eq!(1, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[test_span(tokio::test)]
async fn variables() {
    assert_federated_response!(
        r#"
            query ExampleQuery($topProductsFirst: Int, $reviewsForAuthorAuthorId: ID!) {
                topProducts(first: $topProductsFirst) {
                    name
                    reviewsForAuthor(authorID: $reviewsForAuthorAuthorId) {
                        body
                        author {
                            id
                            name
                        }
                    }
                }
            }
            "#,
        hashmap! {
            "products".to_string()=>1,
            "reviews".to_string()=>1,
            "accounts".to_string()=>1,
        },
    );
}

#[tokio::test]
async fn missing_variables() {
    let request = graphql::Request::builder()
        .query(
            r#"
            query ExampleQuery(
                $missingVariable: Int!,
                $yetAnotherMissingVariable: ID!,
                $notRequiredVariable: Int,
            ) {
                topProducts(first: $missingVariable) {
                    name
                    reviewsForAuthor(authorID: $yetAnotherMissingVariable) {
                        body
                    }
                }
            }
            "#
            .to_string(),
        )
        .build();

    let http_request = http::Request::builder()
        .method("POST")
        .body(request)
        .unwrap()
        .into();

    let request = graphql::RouterRequest {
        context: Context::new().with_request(http_request),
    };
    let (response, _) = query_rust(request).await;
    let expected = vec![
        graphql::FetchError::ValidationInvalidTypeVariable {
            name: "yetAnotherMissingVariable".to_string(),
        }
        .to_graphql_error(None),
        graphql::FetchError::ValidationInvalidTypeVariable {
            name: "missingVariable".to_string(),
        }
        .to_graphql_error(None),
    ];
    assert!(
        response.errors.iter().all(|x| expected.contains(x)),
        "{:?}",
        response.errors
    );
}

async fn query_node(request: &graphql::Request) -> Result<graphql::Response, graphql::FetchError> {
    Ok(reqwest::Client::new()
        .post("http://localhost:4100/graphql")
        .json(request)
        .send()
        .await
        .expect("couldn't send request")
        .json()
        .await
        .expect("couldn't deserialize response"))
}

async fn query_rust(
    request: graphql::RouterRequest,
) -> (graphql::Response, CountingServiceRegistry) {
    let schema = Arc::new(include_str!("fixtures/supergraph.graphql").parse().unwrap());
    let config =
        serde_yaml::from_str::<Configuration>(include_str!("fixtures/supergraph_config.yaml"))
            .unwrap();
    let counting_registry = CountingServiceRegistry::new();
    let mut builder = PluggableRouterServiceBuilder::new(schema, 10);
    for (name, subgraph) in &config.subgraphs {
        let cloned_counter = counting_registry.clone();
        let cloned_name = name.clone();

        let service = ReqwestSubgraphService::new(name.to_owned(), subgraph.routing_url.to_owned())
            .map_request(move |request: SubgraphRequest| {
                let cloned_counter = cloned_counter.clone();
                cloned_counter.increment(cloned_name.as_str());

                request
            });
        builder = builder.with_subgraph_service(name, service);
    }

    builder = builder.with_dispatcher(get_dispatcher());
    let (mut router, _) = builder.build().await;

    let stream = router.ready().await.unwrap().call(request).await.unwrap();
    let (_, response) = stream.response.into_parts();

    match response {
        ResponseBody::GraphQL(response) => (response, counting_registry),
        _ => {
            panic!("Expected graphql response")
        }
    }
}

#[derive(Debug, Clone)]
struct CountingServiceRegistry {
    counts: Arc<Mutex<HashMap<String, usize>>>,
}

impl CountingServiceRegistry {
    fn new() -> CountingServiceRegistry {
        CountingServiceRegistry {
            counts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn increment(&self, service: &str) {
        let mut counts = self.counts.lock().unwrap();
        match counts.entry(service.to_owned()) {
            Entry::Occupied(mut e) => {
                *e.get_mut() += 1;
            }
            Entry::Vacant(e) => {
                e.insert(1);
            }
        };
    }

    fn totals(&self) -> HashMap<String, usize> {
        self.counts.lock().unwrap().clone()
    }
}
