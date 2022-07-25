//!
//! Please ensure that any tests added to this file use the tokio multi-threaded test executor.
//!

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use apollo_router::graphql;
use apollo_router::graphql::Request;
use apollo_router::http_ext;
use apollo_router::json_ext::Object;
use apollo_router::json_ext::ValueExt;
use apollo_router::plugin::Plugin;
use apollo_router::plugins::csrf;
use apollo_router::plugins::telemetry::apollo;
use apollo_router::plugins::telemetry::config::Tracing;
use apollo_router::plugins::telemetry::Telemetry;
use apollo_router::plugins::telemetry::{self};
use apollo_router::services::PluggableRouterServiceBuilder;
use apollo_router::services::RouterRequest;
use apollo_router::services::RouterResponse;
use apollo_router::services::SubgraphRequest;
use apollo_router::services::SubgraphService;
use apollo_router::Context;
use apollo_router::Schema;
use http::Method;
use maplit::hashmap;
use serde_json::to_string_pretty;
use serde_json_bytes::json;
use test_span::prelude::*;
use tower::util::BoxCloneService;
use tower::BoxError;
use tower::ServiceExt;

macro_rules! assert_federated_response {
    ($query:expr, $service_requests:expr $(,)?) => {
        let request = Request::builder()
            .query($query)
            .variables(Object::from_iter(
                vec![
                    ("topProductsFirst".into(), 2.into()),
                    ("reviewsForAuthorAuthorId".into(), 1.into()),
                ]),
            )
            .build();



        let expected = match query_node(&request).await {
            Ok(e) => e,
            Err(err) => {
                panic!("query_node failed: {err}. Probably caused by missing gateway during testing");
            }
        };

        let originating_request = http_ext::Request::fake_builder().method(Method::POST)
            // otherwise the query would be a simple one,
            // and CSRF protection would reject it
            .header("content-type", "application/json")
            .body(request)
            .build().expect("expecting valid originating request");

        let (actual, registry) = query_rust(originating_request.into()).await;

        tracing::debug!("query:\n{}\n", $query);

        assert!(
            expected.data.as_ref().unwrap().is_object(),
            "nodejs: no response's data: please check that the gateway and the subgraphs are running",
        );

        tracing::debug!("expected: {}", to_string_pretty(&expected).unwrap());
        tracing::debug!("actual: {}", to_string_pretty(&actual).unwrap());

        let expected = expected.data.as_ref().expect("expected data should not be none");
        let actual = actual.data.as_ref().expect("received data should not be none");
        assert!(
            expected.eq_and_ordered(actual),
            "the gateway and the router didn't return the same data:\ngateway:\n{}\nrouter\n{}",
            expected,
            actual
        );
        assert_eq!(registry.totals(), $service_requests);
    };
}

#[tokio::test(flavor = "multi_thread")]
async fn basic_request() {
    assert_federated_response!(
        r#"{ topProducts { name name2:name } }"#,
        hashmap! {
            "products".to_string()=>1,
        },
    );
}

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
async fn api_schema_hides_field() {
    let request = Request::builder()
        .query(r#"{ topProducts { name inStock } }"#)
        .variables(Object::from_iter(vec![
            ("topProductsFirst".into(), 2i32.into()),
            ("reviewsForAuthorAuthorId".into(), 1i32.into()),
        ]))
        .build();

    let originating_request = http_ext::Request::fake_builder()
        .method(Method::POST)
        .header("content-type", "application/json")
        .body(request)
        .build()
        .expect("expecting valid request");

    let (actual, _) = query_rust(originating_request.into()).await;

    assert!(actual.errors[0]
        .message
        .as_str()
        .contains("Cannot query field \"inStock\" on type \"Product\"."));
}

#[test_span(tokio::test)]
#[target(apollo_router=tracing::Level::DEBUG)]
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
#[target(apollo_router=tracing::Level::DEBUG)]
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

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
async fn queries_should_work_over_get() {
    let request = Request::builder()
        .query(r#"{ topProducts { upc name reviews {id product { name } author { id name } } } }"#)
        .variables(Object::from_iter(vec![
            ("topProductsFirst".into(), 2.into()),
            ("reviewsForAuthorAuthorId".into(), 1.into()),
        ]))
        .build();

    let expected_service_hits = hashmap! {
        "products".to_string()=>2,
        "reviews".to_string()=>1,
        "accounts".to_string()=>1,
    };

    let originating_request = http_ext::Request::fake_builder()
        .body(request)
        .header("content-type", "application/json")
        .build()
        .expect("expecting valid request");

    let (actual, registry) = query_rust(originating_request.into()).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn simple_queries_should_not_work() {
    let expected_error = graphql::Error {
        message :"This operation has been blocked as a potential Cross-Site Request Forgery (CSRF). \
        Please either specify a 'content-type' header \
        (with a mime-type that is not one of application/x-www-form-urlencoded, multipart/form-data, text/plain) \
        or provide one of the following headers: x-apollo-operation-name, apollo-require-preflight".to_string(),
        ..Default::default()
    };

    let request = Request::builder()
        .query(r#"{ topProducts { upc name reviews {id product { name } author { id name } } } }"#)
        .variables(Object::from_iter(vec![
            ("topProductsFirst".into(), 2.into()),
            ("reviewsForAuthorAuthorId".into(), 1.into()),
        ]))
        .build();

    let originating_request = http_ext::Request::fake_builder()
        .body(request)
        .build()
        .expect("expecting valid request");

    let (actual, registry) = query_rust(originating_request.into()).await;

    assert_eq!(
        1,
        actual.errors.len(),
        "CSRF should have rejected this query"
    );
    assert_eq!(expected_error, actual.errors[0]);
    assert_eq!(registry.totals(), hashmap! {});
}

#[tokio::test(flavor = "multi_thread")]
async fn queries_should_work_with_compression() {
    let request = Request::builder()
        .query(
            r#"{ topProducts { upc name reviews {id product { name } author { id name } } } }"#
                .to_string(),
        )
        .variables(Object::from_iter(vec![
            ("topProductsFirst".into(), 2.into()),
            ("reviewsForAuthorAuthorId".into(), 1.into()),
        ]))
        .build();

    let expected_service_hits = hashmap! {
        "products".to_string()=>2,
        "reviews".to_string()=>1,
        "accounts".to_string()=>1,
    };

    let http_request = http_ext::Request::fake_builder()
        .method(Method::POST)
        .header("content-type", "application/json")
        .header("accept-encoding", "gzip")
        .body(request)
        .build()
        .expect("expecting valid request");

    let request = RouterRequest {
        originating_request: http_request,
        context: Context::new(),
    };

    let (actual, registry) = query_rust(request).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn queries_should_work_over_post() {
    let request = Request::builder()
        .query(r#"{ topProducts { upc name reviews {id product { name } author { id name } } } }"#)
        .variables(Object::from_iter(vec![
            ("topProductsFirst".into(), 2.into()),
            ("reviewsForAuthorAuthorId".into(), 1.into()),
        ]))
        .build();

    let expected_service_hits = hashmap! {
        "products".to_string()=>2,
        "reviews".to_string()=>1,
        "accounts".to_string()=>1,
    };

    let http_request = http_ext::Request::fake_builder()
        .method(Method::POST)
        .header("content-type", "application/json")
        .body(request)
        .build()
        .expect("expecting valid request");

    let request = RouterRequest {
        originating_request: http_request,
        context: Context::new(),
    };

    let (actual, registry) = query_rust(request).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn service_errors_should_be_propagated() {
    let expected_error = apollo_router::graphql::Error {
        message :"value retrieval failed: couldn't plan query: query validation errors: Unknown operation named \"invalidOperationName\"".to_string(),
        ..Default::default()
    };

    let request = Request::builder()
        .query(r#"{ topProducts { name } }"#)
        .operation_name("invalidOperationName")
        .build();

    let expected_service_hits = hashmap! {};

    let originating_request = http_ext::Request::fake_builder()
        .body(request)
        .header("content-type", "application/json")
        .build()
        .expect("expecting valid request");

    let (actual, registry) = query_rust(originating_request.into()).await;

    assert_eq!(expected_error, actual.errors[0]);
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn mutation_should_not_work_over_get() {
    let request = Request::builder()
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
        .variables(Object::from_iter(vec![
            ("topProductsFirst".into(), 2.into()),
            ("reviewsForAuthorAuthorId".into(), 1.into()),
        ]))
        .build();

    // No services should be queried
    let expected_service_hits = hashmap! {};

    let originating_request = http_ext::Request::fake_builder()
        .body(request)
        .header("content-type", "application/json")
        .build()
        .expect("expecting valid request");

    let (actual, registry) = query_rust(originating_request.into()).await;

    assert_eq!(1, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn mutation_should_work_over_post() {
    let request = Request::builder()
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
        .variables(Object::from_iter(vec![
            ("topProductsFirst".into(), 2.into()),
            ("reviewsForAuthorAuthorId".into(), 1.into()),
        ]))
        .build();

    let expected_service_hits = hashmap! {
        "products".to_string()=>1,
        "reviews".to_string()=>2,
    };

    let http_request = http_ext::Request::fake_builder()
        .method(Method::POST)
        .header("content-type", "application/json")
        .body(request)
        .build()
        .expect("expecting valid request");

    let request = RouterRequest {
        originating_request: http_request,
        context: Context::new(),
    };

    let (actual, registry) = query_rust(request).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn automated_persisted_queries() {
    let (router, registry) = setup_router_and_registry().await;

    let mut extensions: Object = Default::default();
    extensions.insert("code", "PERSISTED_QUERY_NOT_FOUND".into());
    extensions.insert(
        "exception",
        json!(
                {"stacktrace":["PersistedQueryNotFoundError: PersistedQueryNotFound"]
        }),
    );
    let expected_apq_miss_error = apollo_router::graphql::Error {
        message: "PersistedQueryNotFound".to_string(),
        extensions,
        ..Default::default()
    };

    let mut request_extensions: Object = Default::default();
    request_extensions.insert(
        "persistedQuery",
        json!({
            "version" : 1u8,
            "sha256Hash" : "9d1474aa069127ff795d3412b11dfc1f1be0853aed7a54c4a619ee0b1725382e"
        }),
    );
    let request_builder = Request::builder().extensions(request_extensions.clone());
    let apq_only_request = Request::builder()
        .extensions(request_extensions.clone())
        .build();

    // First query, apq hash but no query, it will be a cache miss.

    // No services should be queried
    let expected_service_hits = hashmap! {};

    let originating_request = http_ext::Request::fake_builder()
        .body(apq_only_request)
        .header("content-type", "application/json")
        .build()
        .expect("expecting valid request");

    let actual = query_with_router(router.clone(), originating_request.into()).await;

    assert_eq!(expected_apq_miss_error, actual.errors[0]);
    assert_eq!(1, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);

    // Second query, apq hash with corresponding query, it will be inserted into the cache.

    let apq_request_with_query = Request::builder()
        .extensions(request_extensions.clone())
        .query("query Query { me { name } }")
        .build();

    // Services should have been queried once
    let expected_service_hits = hashmap! {
        "accounts".to_string()=>1,
    };

    let originating_request = http_ext::Request::fake_builder()
        .body(apq_request_with_query)
        .header("content-type", "application/json")
        .build()
        .expect("expecting valid request");

    let actual = query_with_router(router.clone(), originating_request.into()).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);

    // Third and last query, apq hash without query, it will trigger an apq cache hit.
    let apq_only_request = request_builder.build();

    // Services should have been queried twice
    let expected_service_hits = hashmap! {
        "accounts".to_string()=>2,
    };

    let originating_request = http_ext::Request::fake_builder()
        .body(apq_only_request)
        .header("content-type", "application/json")
        .build()
        .expect("expecting valid request");

    let actual = query_with_router(router, originating_request.into()).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[test_span(tokio::test(flavor = "multi_thread"))]
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

#[tokio::test(flavor = "multi_thread")]
async fn missing_variables() {
    let request = Request::builder()
        .query(
            r#"
            query ExampleQuery(
                $missingVariable: Int!,
                $yetAnotherMissingVariable: ID!,
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

    let originating_request = http_ext::Request::fake_builder()
        .method(Method::POST)
        .header("content-type", "application/json")
        .body(request)
        .build()
        .expect("expecting valid request");

    let (response, _) = query_rust(originating_request.into()).await;
    let expected = vec![
        apollo_router::error::FetchError::ValidationInvalidTypeVariable {
            name: "yetAnotherMissingVariable".to_string(),
        }
        .to_graphql_error(None),
        apollo_router::error::FetchError::ValidationInvalidTypeVariable {
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

async fn query_node(
    request: &graphql::Request,
) -> Result<graphql::Response, apollo_router::error::FetchError> {
    reqwest::Client::new()
        .post("https://federation-demo-gateway.fly.dev/")
        .json(request)
        .send()
        .await
        .map_err(
            |err| apollo_router::error::FetchError::SubrequestHttpError {
                service: "test node".to_string(),
                reason: err.to_string(),
            },
        )?
        .json()
        .await
        .map_err(
            |err| apollo_router::error::FetchError::SubrequestMalformedResponse {
                service: "test node".to_string(),
                reason: err.to_string(),
            },
        )
}

async fn query_rust(
    request: RouterRequest,
) -> (apollo_router::graphql::Response, CountingServiceRegistry) {
    let (router, counting_registry) = setup_router_and_registry().await;
    (query_with_router(router, request).await, counting_registry)
}

async fn setup_router_and_registry() -> (
    BoxCloneService<RouterRequest, RouterResponse, BoxError>,
    CountingServiceRegistry,
) {
    let schema: Arc<Schema> =
        Arc::new(include_str!("fixtures/supergraph.graphql").parse().unwrap());
    let counting_registry = CountingServiceRegistry::new();
    let subgraphs = schema.subgraphs();
    let mut builder = PluggableRouterServiceBuilder::new(schema.clone());
    let telemetry_plugin = Telemetry::new(telemetry::config::Conf {
        metrics: Option::default(),
        tracing: Some(Tracing::default()),
        apollo: Some(apollo::Config::default()),
    })
    .await
    .unwrap();
    let csrf_plugin = csrf::Csrf::new(Default::default()).await.unwrap();
    builder = builder
        .with_dyn_plugin("apollo.telemetry".to_string(), Box::new(telemetry_plugin))
        .with_dyn_plugin("apollo.csrf".to_string(), Box::new(csrf_plugin));
    for (name, _url) in subgraphs {
        let cloned_counter = counting_registry.clone();
        let cloned_name = name.clone();

        let service =
            SubgraphService::new(name.to_owned()).map_request(move |request: SubgraphRequest| {
                let cloned_counter = cloned_counter.clone();
                cloned_counter.increment(cloned_name.as_str());

                request
            });
        builder = builder.with_subgraph_service(name, service);
    }

    let router = builder.build().await.unwrap().test_service();

    (router, counting_registry)
}

async fn query_with_router(
    router: BoxCloneService<RouterRequest, RouterResponse, BoxError>,
    request: RouterRequest,
) -> graphql::Response {
    router
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
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
