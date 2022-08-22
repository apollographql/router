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
use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInit;
use apollo_router::services::subgraph;
use apollo_router::services::supergraph;
use apollo_router::Context;
use apollo_router::_private::TelemetryPlugin;
use http::Method;
use http::StatusCode;
use maplit::hashmap;
use serde_json::to_string_pretty;
use serde_json_bytes::json;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use test_span::prelude::*;
use tower::BoxError;
use tower::ServiceExt;
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;

type Object = Map<ByteString, Value>;

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

    let request = supergraph::Request {
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

    let request = supergraph::Request {
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

    let request = supergraph::Request {
        originating_request: http_request,
        context: Context::new(),
    };

    let (actual, registry) = query_rust(request).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn automated_persisted_queries() {
    let (router, registry) = setup_router_and_registry(serde_json::json!({})).await;

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

    let (mut http_response, _) = http_query_rust(originating_request.into()).await;

    assert_eq!(StatusCode::BAD_REQUEST, http_response.response.status());

    let response = http_response.next_response().await.unwrap();
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

#[tokio::test(flavor = "multi_thread")]
async fn query_just_under_recursion_limit() {
    let config = serde_json::json!({
        "server": {"experimental_parser_recursion_limit": 12_usize}
    });
    let request = Request::builder()
        .query(r#"{ me { reviews { author { reviews { author { name } } } } } }"#)
        .build();

    let expected_service_hits = hashmap! {
        "reviews".to_string() => 1,
        "accounts".to_string() => 2,
    };

    let originating_request = http_ext::Request::fake_builder()
        .body(request)
        .header("content-type", "application/json")
        .build()
        .expect("expecting valid request");

    let (actual, registry) = query_rust_with_config(originating_request.into(), config).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn query_just_at_recursion_limit() {
    let config = serde_json::json!({
        "server": {"experimental_parser_recursion_limit": 11_usize}
    });
    let request = Request::builder()
        .query(r#"{ me { reviews { author { reviews { author { name } } } } } }"#)
        .build();

    let expected_service_hits = hashmap! {};

    let originating_request = http_ext::Request::fake_builder()
        .body(request)
        .header("content-type", "application/json")
        .build()
        .expect("expecting valid request");

    let (actual, registry) = query_rust_with_config(originating_request.into(), config).await;

    assert_eq!(1, actual.errors.len());
    assert!(actual.errors[0]
        .message
        .contains("parser limit(11) reached"));
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn defer_path() {
    let config = serde_json::json!({
        "server": {
            "experimental_defer_support": true
        },
        "plugins": {
            "experimental.include_subgraph_errors": {
                "all": true
            }
        }
    });
    let request = Request::builder()
        .query(
            r#"{
            me {
                id
                ...@defer(label: "name") {
                    name
                }
            }
        }"#,
        )
        .build();

    let request = http_ext::Request::fake_builder()
        .body(request)
        .header("content-type", "application/json")
        .build()
        .expect("expecting valid request");

    let (router, _) = setup_router_and_registry(config).await;

    let mut stream = router.oneshot(request.into()).await.unwrap();

    let first = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(first);

    let second = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(second);
}

#[tokio::test(flavor = "multi_thread")]
async fn defer_path_in_array() {
    let config = serde_json::json!({
        "server": {
            "experimental_defer_support": true
        },
        "plugins": {
            "experimental.include_subgraph_errors": {
                "all": true
            }
        }
    });
    let request = Request::builder()
        .query(
            r#"{
                me {
                    reviews {
                        id
                        author {
                            id
                            ... @defer(label: "author name") {
                            name
                            }
                        }
                    }
                }
            }"#,
        )
        .build();

    let request = http_ext::Request::fake_builder()
        .body(request)
        .header("content-type", "application/json")
        .build()
        .expect("expecting valid request");

    let (router, _) = setup_router_and_registry(config).await;

    let mut stream = router.oneshot(request.into()).await.unwrap();

    let first = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(first);

    let second = stream.next_response().await.unwrap();
    insta::assert_json_snapshot!(second);
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

async fn http_query_rust(
    request: supergraph::Request,
) -> (supergraph::Response, CountingServiceRegistry) {
    http_query_rust_with_config(request, serde_json::json!({})).await
}

async fn query_rust(
    request: supergraph::Request,
) -> (apollo_router::graphql::Response, CountingServiceRegistry) {
    query_rust_with_config(request, serde_json::json!({})).await
}

async fn http_query_rust_with_config(
    request: supergraph::Request,
    config: serde_json::Value,
) -> (supergraph::Response, CountingServiceRegistry) {
    let (router, counting_registry) = setup_router_and_registry(config).await;
    (
        http_query_with_router(router, request).await,
        counting_registry,
    )
}

async fn query_rust_with_config(
    request: supergraph::Request,
    config: serde_json::Value,
) -> (apollo_router::graphql::Response, CountingServiceRegistry) {
    let (router, counting_registry) = setup_router_and_registry(config).await;
    (query_with_router(router, request).await, counting_registry)
}

async fn setup_router_and_registry(
    config: serde_json::Value,
) -> (supergraph::BoxCloneService, CountingServiceRegistry) {
    let config = serde_json::from_value(config).unwrap();
    let counting_registry = CountingServiceRegistry::new();
    let telemetry = TelemetryPlugin::new_with_subscriber(
        serde_json::json!({
            "tracing": {},
            "apollo": {
                "schema_id": ""
            }
        }),
        tracing_subscriber::registry().with(test_span::Layer {}),
    )
    .await
    .unwrap();
    let router = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(config)
        .unwrap()
        .schema(include_str!("fixtures/supergraph.graphql"))
        .extra_plugin(counting_registry.clone())
        .extra_plugin(telemetry)
        .build()
        .await
        .unwrap();
    (router, counting_registry)
}

async fn query_with_router(
    router: supergraph::BoxCloneService,
    request: supergraph::Request,
) -> graphql::Response {
    router
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
}

async fn http_query_with_router(
    router: supergraph::BoxCloneService,
    request: supergraph::Request,
) -> supergraph::Response {
    router.oneshot(request).await.unwrap()
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

#[async_trait::async_trait]
impl Plugin for CountingServiceRegistry {
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!()
    }

    fn subgraph_service(
        &self,
        subgraph_name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService {
        let name = subgraph_name.to_owned();
        let counters = self.clone();
        service
            .map_request(move |request| {
                counters.increment(&name);
                request
            })
            .boxed()
    }
}

trait ValueExt {
    fn eq_and_ordered(&self, other: &Self) -> bool;
}

impl ValueExt for Value {
    fn eq_and_ordered(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Object(a), Value::Object(b)) => {
                let mut it_a = a.iter();
                let mut it_b = b.iter();

                loop {
                    match (it_a.next(), it_b.next()) {
                        (Some(_), None) | (None, Some(_)) => break false,
                        (None, None) => break true,
                        (Some((field_a, value_a)), Some((field_b, value_b)))
                            if field_a == field_b && ValueExt::eq_and_ordered(value_a, value_b) =>
                        {
                            continue
                        }
                        (Some(_), Some(_)) => break false,
                    }
                }
            }
            (Value::Array(a), Value::Array(b)) => {
                let mut it_a = a.iter();
                let mut it_b = b.iter();

                loop {
                    match (it_a.next(), it_b.next()) {
                        (Some(_), None) | (None, Some(_)) => break false,
                        (None, None) => break true,
                        (Some(value_a), Some(value_b))
                            if ValueExt::eq_and_ordered(value_a, value_b) =>
                        {
                            continue
                        }
                        (Some(_), Some(_)) => break false,
                    }
                }
            }
            (a, b) => a == b,
        }
    }
}
