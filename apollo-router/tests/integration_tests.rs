//!
//! Please ensure that any tests added to this file use the tokio multi-threaded test executor.
//!

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use apollo_router::graphql;
use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInit;
use apollo_router::services::router;
use apollo_router::services::subgraph;
use apollo_router::services::supergraph;
use apollo_router::Context;
use futures::StreamExt;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use http::Method;
use http::StatusCode;
use http::Uri;
use maplit::hashmap;
use mime::APPLICATION_JSON;
use serde_json::to_string_pretty;
use serde_json_bytes::json;
use serde_json_bytes::Value;
use tower::BoxError;
use tower::ServiceExt;

macro_rules! assert_federated_response {
    ($query:expr, $service_requests:expr $(,)?) => {
        let request = supergraph::Request::fake_builder()
            .query($query)
            .variable("topProductsFirst", 2_i32)
            .variable("reviewsForAuthorAuthorId", 1_i32)
            .method(Method::POST)
            .build()
            .unwrap();

        let expected = match query_node(&request).await {
            Ok(e) => e,
            Err(err) => {
                panic!("query_node failed: {err}. Probably caused by missing gateway during testing");
            }
        };
        assert_eq!(expected.errors, []);

        let (actual, registry) = query_rust(request).await;
        assert_eq!(actual.errors, []);

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
    let request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts { name inStock } }"#)
        .variable("topProductsFirst", 2_i32)
        .variable("reviewsForAuthorAuthorId", 1_i32)
        .build()
        .expect("expecting valid request");

    let (actual, _) = query_rust(request).await;

    assert!(actual.errors[0]
        .message
        .as_str()
        .contains("Cannot query field \"inStock\" on type \"Product\"."));
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
    // get request
    let get_request = supergraph::Request::builder()
        .query("{ topProducts { upc name reviews {id product { name } author { id name } } } }")
        .variable("topProductsFirst", 2_usize)
        .variable("reviewsForAuthorAuthorId", 1_usize)
        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
        .uri(Uri::from_static("/"))
        .method(Method::GET)
        .context(Context::new())
        .build()
        .unwrap()
        .try_into()
        .unwrap();

    let expected_service_hits = hashmap! {
        "products".to_string()=>2,
        "reviews".to_string()=>1,
        "accounts".to_string()=>1,
    };

    let (actual, registry) = {
        let (router, counting_registry) = setup_router_and_registry(serde_json::json!({})).await;
        (
            query_with_router(router, get_request).await,
            counting_registry,
        )
    };
    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn simple_queries_should_not_work() {
    let message = "This operation has been blocked as a potential Cross-Site Request Forgery (CSRF). \
    Please either specify a 'content-type' header \
    (with a mime-type that is not one of application/x-www-form-urlencoded, multipart/form-data, text/plain) \
    or provide one of the following headers: x-apollo-operation-name, apollo-require-preflight";
    let expected_error = graphql::Error::builder()
        .message(message)
        .extension_code("CSRF_ERROR")
        .build();

    let mut get_request: router::Request = supergraph::Request::builder()
        .query("{ topProducts { upc name reviews {id product { name } author { id name } } } }")
        .variable("topProductsFirst", 2_usize)
        .variable("reviewsForAuthorAuthorId", 1_usize)
        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
        .uri(Uri::from_static("/"))
        .method(Method::GET)
        .context(Context::new())
        .build()
        .unwrap()
        .try_into()
        .unwrap();

    get_request
        .router_request
        .headers_mut()
        .remove("content-type");

    let (router, registry) = setup_router_and_registry(serde_json::json!({})).await;

    let actual = query_with_router(router, get_request).await;

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
    let request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts { upc name reviews {id product { name } author { id name } } } }"#)
        .variable("topProductsFirst", 2_i32)
        .variable("reviewsForAuthorAuthorId", 1_i32)
        .method(Method::POST)
        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
        .header("accept-encoding", "gzip")
        .build()
        .expect("expecting valid request");

    let expected_service_hits = hashmap! {
        "products".to_string()=>2,
        "reviews".to_string()=>1,
        "accounts".to_string()=>1,
    };

    let (actual, registry) = query_rust(request).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn queries_should_work_over_post() {
    let request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts { upc name reviews {id product { name } author { id name } } } }"#)
        .variable("topProductsFirst", 2_i32)
        .variable("reviewsForAuthorAuthorId", 1_i32)
        .method(Method::POST)
        .build()
        .expect("expecting valid request");

    let expected_service_hits = hashmap! {
        "products".to_string()=>2,
        "reviews".to_string()=>1,
        "accounts".to_string()=>1,
    };

    let (actual, registry) = query_rust(request).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn service_errors_should_be_propagated() {
    let message = "Unknown operation named \"invalidOperationName\"";
    let mut extensions_map = serde_json_bytes::map::Map::new();
    extensions_map.insert("code", "GRAPHQL_VALIDATION_FAILED".into());
    let expected_error = apollo_router::graphql::Error::builder()
        .message(message)
        .extensions(extensions_map)
        .extension_code("VALIDATION_ERROR")
        .build();

    let request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts { name } }"#)
        .operation_name("invalidOperationName")
        .build()
        .expect("expecting valid request");

    let expected_service_hits = hashmap! {};

    let (actual, registry) = query_rust(request).await;

    assert_eq!(expected_error, actual.errors[0]);
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn mutation_should_not_work_over_get() {
    // get request
    let get_request: router::Request = supergraph::Request::builder()
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
        .variable("topProductsFirst", 2_usize)
        .variable("reviewsForAuthorAuthorId", 1_usize)
        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
        .uri(Uri::from_static("/"))
        .method(Method::GET)
        .context(Context::new())
        .build()
        .unwrap()
        .try_into()
        .unwrap();

    // No services should be queried
    let expected_service_hits = hashmap! {};

    let (actual, registry) = {
        let (router, counting_registry) = setup_router_and_registry(serde_json::json!({})).await;
        (
            query_with_router(router, get_request).await,
            counting_registry,
        )
    };

    assert_eq!(1, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn mutation_should_work_over_post() {
    let request = supergraph::Request::fake_builder()
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
        .variable("topProductsFirst", 2_i32)
        .variable("reviewsForAuthorAuthorId", 1_i32)
        .method(Method::POST)
        .build()
        .expect("expecting valid request");

    let expected_service_hits = hashmap! {
        "products".to_string()=>1,
        "reviews".to_string()=>2,
    };

    let (actual, registry) = query_rust(request).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn automated_persisted_queries() {
    let (router, registry) = setup_router_and_registry(serde_json::json!({})).await;

    let expected_apq_miss_error = apollo_router::graphql::Error::builder()
        .message("PersistedQueryNotFound")
        .extension(
            "exception",
            json!({
                "stacktrace": ["PersistedQueryNotFoundError: PersistedQueryNotFound"]
            }),
        )
        .extension_code("PERSISTED_QUERY_NOT_FOUND")
        .build();

    let persisted = json!({
        "version" : 1u8,
        "sha256Hash" : "9d1474aa069127ff795d3412b11dfc1f1be0853aed7a54c4a619ee0b1725382e"
    });

    let apq_only_request = supergraph::Request::fake_builder()
        .extension("persistedQuery", persisted.clone())
        .build()
        .expect("expecting valid request");

    // First query, apq hash but no query, it will be a cache miss.

    // No services should be queried
    let expected_service_hits = hashmap! {};

    let actual = query_with_router(router.clone(), apq_only_request.try_into().unwrap()).await;

    assert_eq!(expected_apq_miss_error, actual.errors[0]);
    assert_eq!(1, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);

    // Second query, apq hash with corresponding query, it will be inserted into the cache.

    let apq_request_with_query = supergraph::Request::fake_builder()
        .extension("persistedQuery", persisted.clone())
        .query("query Query { me { name } }")
        .build()
        .expect("expecting valid request");

    // Services should have been queried once
    let expected_service_hits = hashmap! {
        "accounts".to_string()=>1,
    };

    let actual =
        query_with_router(router.clone(), apq_request_with_query.try_into().unwrap()).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);

    // Third and last query, apq hash without query, it will trigger an apq cache hit.
    let apq_only_request = supergraph::Request::fake_builder()
        .extension("persistedQuery", persisted)
        .build()
        .expect("expecting valid request");

    // Services should have been queried twice
    let expected_service_hits = hashmap! {
        "accounts".to_string()=>2,
    };

    let actual = query_with_router(router, apq_only_request.try_into().unwrap()).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_variables() {
    let request = supergraph::Request::fake_builder()
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
        .method(Method::POST)
        .build()
        .expect("expecting valid request");

    let (mut http_response, _) = http_query_rust(request).await;

    assert_eq!(StatusCode::BAD_REQUEST, http_response.response.status());

    let mut response = serde_json::from_slice::<graphql::Response>(
        http_response
            .next_response()
            .await
            .unwrap()
            .unwrap()
            .to_vec()
            .as_slice(),
    )
    .unwrap();

    let mut expected = vec![
        graphql::Error::builder()
            .message("invalid type for variable: 'missingVariable'")
            .extension_code("VALIDATION_INVALID_TYPE_VARIABLE")
            .extension("name", "missingVariable")
            .build(),
        graphql::Error::builder()
            .message("invalid type for variable: 'yetAnotherMissingVariable'")
            .extension_code("VALIDATION_INVALID_TYPE_VARIABLE")
            .extension("name", "yetAnotherMissingVariable")
            .build(),
    ];
    response.errors.sort_by_key(|e| e.message.clone());
    expected.sort_by_key(|e| e.message.clone());
    assert_eq!(response.errors, expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn query_just_under_recursion_limit() {
    let config = serde_json::json!({
        "server": {"experimental_parser_recursion_limit": 12_usize}
    });
    let request = supergraph::Request::fake_builder()
        .query(r#"{ me { reviews { author { reviews { author { name } } } } } }"#)
        .build()
        .expect("expecting valid request");

    let expected_service_hits = hashmap! {
        "reviews".to_string() => 1,
        "accounts".to_string() => 2,
    };

    let (actual, registry) = query_rust_with_config(request, config).await;

    assert_eq!(0, actual.errors.len());
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn query_just_at_recursion_limit() {
    let config = serde_json::json!({
        "server": {"experimental_parser_recursion_limit": 11_usize}
    });
    let request = supergraph::Request::fake_builder()
        .query(r#"{ me { reviews { author { reviews { author { name } } } } } }"#)
        .build()
        .expect("expecting valid request");

    let expected_service_hits = hashmap! {};

    let (actual, registry) = query_rust_with_config(request, config).await;

    assert_eq!(1, actual.errors.len());
    assert!(actual.errors[0]
        .message
        .contains("parser limit(11) reached"));
    assert_eq!(registry.totals(), expected_service_hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn normal_query_with_defer_accept_header() {
    let request = supergraph::Request::fake_builder()
        .query(r#"{ me { reviews { author { reviews { author { name } } } } } }"#)
        .header(ACCEPT, "multipart/mixed; deferSpec=20220824")
        .build()
        .expect("expecting valid request");
    let (mut response, _registry) = {
        let (router, counting_registry) = setup_router_and_registry(serde_json::json!({})).await;
        (
            router
                .oneshot(request.try_into().unwrap())
                .await
                .unwrap()
                .into_graphql_response_stream()
                .await,
            counting_registry,
        )
    };
    insta::assert_json_snapshot!(response.next().await.unwrap().unwrap());
    assert!(response.next().await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn defer_path_with_disabled_config() {
    let config = serde_json::json!({
        "supergraph": {
            "defer_support": false,
        },
        "plugins": {
            "apollo.include_subgraph_errors": {
                "all": true
            }
        }
    });
    let request = supergraph::Request::fake_builder()
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
        .header(ACCEPT, "multipart/mixed; deferSpec=20220824")
        .build()
        .expect("expecting failure due to disabled config defer support");

    let (router, _) = setup_router_and_registry(config).await;

    let mut stream = router
        .oneshot(request.try_into().unwrap())
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await;

    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    assert!(stream.next().await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn defer_path() {
    let config = serde_json::json!({
        "plugins": {
            "apollo.include_subgraph_errors": {
                "all": true
            }
        }
    });
    let request = supergraph::Request::fake_builder()
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
        .header(ACCEPT, "multipart/mixed; deferSpec=20220824")
        .build()
        .expect("expecting valid request");

    let (router, _) = setup_router_and_registry(config).await;

    let mut stream = router
        .oneshot(request.try_into().unwrap())
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await;

    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    assert!(stream.next().await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn defer_path_in_array() {
    let config = serde_json::json!({
        "plugins": {
            "apollo.include_subgraph_errors": {
                "all": true
            }
        }
    });
    let request = supergraph::Request::fake_builder()
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
        .header(ACCEPT, "multipart/mixed; deferSpec=20220824")
        .build()
        .expect("expecting valid request");

    let (router, _) = setup_router_and_registry(config).await;

    let mut stream = router
        .oneshot(request.try_into().unwrap())
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await;

    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    assert!(stream.next().await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn defer_query_without_accept() {
    let config = serde_json::json!({
        "plugins": {
            "apollo.include_subgraph_errors": {
                "all": true
            }
        }
    });
    let request = supergraph::Request::fake_builder()
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
        .header(ACCEPT, APPLICATION_JSON.essence_str())
        .build()
        .expect("expecting valid request");

    let (router, _) = setup_router_and_registry(config).await;

    let mut stream = router.oneshot(request.try_into().unwrap()).await.unwrap();
    let first = stream.next_response().await.unwrap().unwrap();
    insta::assert_snapshot!(std::str::from_utf8(first.to_vec().as_slice()).unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn defer_empty_primary_response() {
    let config = serde_json::json!({
        "plugins": {
            "apollo.include_subgraph_errors": {
                "all": true
            }
        }
    });
    let request = supergraph::Request::fake_builder()
        .query(
            r#"{
            me {
                ...@defer(label: "name") {
                    name
                }
            }
        }"#,
        )
        .header(ACCEPT, "multipart/mixed; deferSpec=20220824")
        .build()
        .expect("expecting valid request");

    let (router, _) = setup_router_and_registry(config).await;

    let mut stream = router
        .oneshot(request.try_into().unwrap())
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await;

    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    assert!(stream.next().await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn defer_default_variable() {
    let config = serde_json::json!({
        "include_subgraph_errors": {
            "all": true
        }
    });

    let query = r#"query X($if: Boolean! = true){
        me {
            id
            ...@defer(label: "name", if: $if) {
                name
            }
        }
    }"#;

    let request = supergraph::Request::fake_builder()
        .query(query)
        .header(ACCEPT, "multipart/mixed; deferSpec=20220824")
        .build()
        .expect("expecting valid request");

    let (router, _) = setup_router_and_registry(config.clone()).await;

    let mut stream = router
        .oneshot(request.try_into().unwrap())
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await;

    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    assert!(stream.next().await.is_none());

    let request = supergraph::Request::fake_builder()
        .query(query)
        .variable("if", false)
        .header(ACCEPT, "multipart/mixed; deferSpec=20220824")
        .build()
        .expect("expecting valid request");

    let (router, _) = setup_router_and_registry(config).await;

    let mut stream = router
        .oneshot(request.try_into().unwrap())
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await;

    insta::assert_json_snapshot!(stream.next().await.unwrap().unwrap());
    assert!(stream.next().await.is_none());
}

async fn query_node(request: &supergraph::Request) -> Result<graphql::Response, String> {
    reqwest::Client::new()
        .post("https://federation-demo-gateway.fly.dev/")
        .json(request.supergraph_request.body())
        .send()
        .await
        .map_err(|err| format!("HTTP fetch failed from 'test node': {err}"))?
        .json()
        .await
        .map_err(|err| format!("service 'test node' response was malformed: {err}"))
}

async fn http_query_rust(
    request: supergraph::Request,
) -> (router::Response, CountingServiceRegistry) {
    http_query_rust_with_config(request, serde_json::json!({})).await
}

async fn query_rust(
    request: supergraph::Request,
) -> (apollo_router::graphql::Response, CountingServiceRegistry) {
    query_rust_with_config(
        request,
        serde_json::json!({
            "telemetry":{
              "apollo": {
                    "field_level_instrumentation_sampler": "always_off"
                }
            }
        }),
    )
    .await
}

async fn http_query_rust_with_config(
    request: supergraph::Request,
    config: serde_json::Value,
) -> (router::Response, CountingServiceRegistry) {
    let (router, counting_registry) = setup_router_and_registry(config).await;
    (
        http_query_with_router(router, request.try_into().unwrap()).await,
        counting_registry,
    )
}

async fn query_rust_with_config(
    request: supergraph::Request,
    config: serde_json::Value,
) -> (apollo_router::graphql::Response, CountingServiceRegistry) {
    let (router, counting_registry) = setup_router_and_registry(config).await;
    (
        query_with_router(router, request.try_into().unwrap()).await,
        counting_registry,
    )
}

async fn setup_router_and_registry(
    config: serde_json::Value,
) -> (router::BoxCloneService, CountingServiceRegistry) {
    let counting_registry = CountingServiceRegistry::new();
    let router = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(config)
        .unwrap()
        .schema(include_str!("fixtures/supergraph.graphql"))
        .extra_plugin(counting_registry.clone())
        .build_router()
        .await
        .unwrap();
    (router, counting_registry)
}

async fn query_with_router(
    router: router::BoxCloneService,
    request: router::Request,
) -> graphql::Response {
    serde_json::from_slice(
        router
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .unwrap()
            .to_vec()
            .as_slice(),
    )
    .unwrap()
}

async fn http_query_with_router(
    router: router::BoxCloneService,
    request: router::Request,
) -> router::Response {
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
