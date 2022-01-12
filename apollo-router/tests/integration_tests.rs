use apollo_router::configuration::Configuration;
use apollo_router::http_service_registry::HttpServiceRegistry;
use apollo_router::http_subgraph::HttpSubgraphFetcher;
use apollo_router::ApolloRouter;
use apollo_router_core::prelude::*;
use apollo_router_core::ValueExt;
use maplit::hashmap;
use serde_json::to_string_pretty;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use test_span::prelude::*;
use url::Url;

macro_rules! assert_federated_response {
    ($query:expr, $service_requests:expr $(,)?) => {
        let request = graphql::Request::builder()
            .query($query)
            .variables(Arc::new(
                vec![
                    ("topProductsFirst".to_string(), 2.into()),
                    ("reviewsForAuthorAuthorId".to_string(), 1.into()),
                ]
                .into_iter()
                .collect(),
            ))
            .build();
        let (actual, registry) = query_rust(request.clone()).await;
        let expected = query_node(request.clone()).await.unwrap();

        tracing::debug!("query:\n{}\n", request.query.as_str());

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
async fn traced_basic_request() {
    assert_federated_response!(
        r#"{ topProducts { name name2:name } }"#,
        hashmap! {
            "products".to_string()=>1,
        },
    );
    insta::assert_json_snapshot!(get_spans());
}

#[test_span(tokio::test)]
async fn traced_basic_composition() {
    assert_federated_response!(
        r#"{ topProducts { upc name reviews {id product { name } author { id name } } } }"#,
        hashmap! {
            "products".to_string()=>2,
            "reviews".to_string()=>1,
            "accounts".to_string()=>1,
        },
    );
    insta::assert_json_snapshot!(get_spans());
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
            "#,
        )
        .build();
    let (response, _) = query_rust(request.clone()).await;
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

async fn query_node(request: graphql::Request) -> Result<graphql::Response, graphql::FetchError> {
    let nodejs_impl = HttpSubgraphFetcher::new(
        "federated",
        Url::parse("http://localhost:4100/graphql").unwrap(),
    );
    nodejs_impl.stream(request).await
}

async fn query_rust(
    request: graphql::Request,
) -> (graphql::Response, Arc<CountingServiceRegistry>) {
    let schema = Arc::new(include_str!("fixtures/supergraph.graphql").parse().unwrap());
    let config =
        serde_yaml::from_str::<Configuration>(include_str!("fixtures/supergraph_config.yaml"))
            .unwrap();
    let registry = Arc::new(CountingServiceRegistry::new(HttpServiceRegistry::new(
        &config,
    )));

    let router = ApolloRouter::new(registry.clone(), schema, None).await;

    let request = Arc::new(request);
    let stream = match router.prepare_query(request.clone()).await {
        Ok(route) => route.execute(request).await,
        Err(stream) => stream,
    };

    (stream, registry)
}

#[derive(Debug)]
struct CountingServiceRegistry {
    counts: Arc<Mutex<HashMap<String, usize>>>,
    delegate: HttpServiceRegistry,
}

impl CountingServiceRegistry {
    fn new(delegate: HttpServiceRegistry) -> CountingServiceRegistry {
        CountingServiceRegistry {
            counts: Arc::new(Mutex::new(HashMap::new())),
            delegate,
        }
    }

    fn totals(&self) -> HashMap<String, usize> {
        self.counts.lock().unwrap().clone()
    }
}

impl ServiceRegistry for CountingServiceRegistry {
    fn get(&self, service: &str) -> Option<&dyn graphql::Fetcher> {
        let mut counts = self.counts.lock().unwrap();
        match counts.entry(service.to_owned()) {
            Entry::Occupied(mut e) => {
                *e.get_mut() += 1;
            }
            Entry::Vacant(e) => {
                e.insert(1);
            }
        }
        self.delegate.get(service)
    }

    fn has(&self, service: &str) -> bool {
        self.delegate.has(service)
    }
}
