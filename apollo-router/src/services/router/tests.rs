use std::sync::Arc;
use std::sync::Mutex;

use futures::stream::StreamExt;
use http::header::CONTENT_TYPE;
use http::header::VARY;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use http::Uri;
use mime::APPLICATION_JSON;
use serde_json_bytes::json;
use tower::ServiceExt;
use tower_service::Service;

use crate::graphql;
use crate::services::router;
use crate::services::router::service::from_supergraph_mock_callback;
use crate::services::router::service::process_vary_header;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::services::MULTIPART_DEFER_CONTENT_TYPE;
use crate::Context;

// Test Vary processing

#[test]
fn it_adds_default_with_value_origin_if_no_vary_header() {
    let mut default_headers = HeaderMap::new();
    process_vary_header(&mut default_headers);
    let vary_opt = default_headers.get(VARY);
    assert!(vary_opt.is_some());
    let vary = vary_opt.expect("has a value");
    assert_eq!(vary, "origin");
}

#[test]
fn it_leaves_vary_alone_if_set() {
    let mut default_headers = HeaderMap::new();
    default_headers.insert(VARY, HeaderValue::from_static("*"));
    process_vary_header(&mut default_headers);
    let vary_opt = default_headers.get(VARY);
    assert!(vary_opt.is_some());
    let vary = vary_opt.expect("has a value");
    assert_eq!(vary, "*");
}

#[test]
fn it_leaves_varys_alone_if_there_are_more_than_one() {
    let mut default_headers = HeaderMap::new();
    default_headers.insert(VARY, HeaderValue::from_static("one"));
    default_headers.append(VARY, HeaderValue::from_static("two"));
    process_vary_header(&mut default_headers);
    let vary = default_headers.get_all(VARY);
    assert_eq!(vary.iter().count(), 2);
    for value in vary {
        assert!(value == "one" || value == "two");
    }
}

#[tokio::test]
async fn it_extracts_query_and_operation_name() {
    let query = "query";
    let expected_query = query;
    let operation_name = "operationName";
    let expected_operation_name = operation_name;

    let expected_response = graphql::Response::builder()
        .data(json!({"response": "yay"}))
        .build();

    let mut router_service = from_supergraph_mock_callback(move |req| {
        let example_response = expected_response.clone();

        assert_eq!(
            req.supergraph_request.body().query.as_deref().unwrap(),
            expected_query
        );
        assert_eq!(
            req.supergraph_request
                .body()
                .operation_name
                .as_deref()
                .unwrap(),
            expected_operation_name
        );

        Ok(SupergraphResponse::new_from_graphql_response(
            example_response,
            req.context,
        ))
    })
    .await;

    // get request
    let get_request = supergraph::Request::builder()
        .query(query)
        .operation_name(operation_name)
        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
        .uri(Uri::from_static("/"))
        .method(Method::GET)
        .context(Context::new())
        .build()
        .unwrap()
        .try_into()
        .unwrap();

    router_service.call(get_request).await.unwrap();

    // post request
    let post_request = supergraph::Request::builder()
        .query(query)
        .operation_name(operation_name)
        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
        .uri(Uri::from_static("/"))
        .method(Method::POST)
        .context(Context::new())
        .build()
        .unwrap();

    router_service
        .call(post_request.try_into().unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn it_fails_on_empty_query() {
    let expected_error = "Must provide query string.";

    let router_service = from_supergraph_mock_callback(move |_req| unreachable!()).await;

    let request = SupergraphRequest::fake_builder()
        .query("".to_string())
        .build()
        .expect("expecting valid request")
        .try_into()
        .unwrap();

    let response = router_service
        .oneshot(request)
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await
        .next()
        .await
        .unwrap()
        .unwrap();
    let actual_error = response.errors[0].message.clone();

    assert_eq!(expected_error, actual_error);
    assert!(response.errors[0].extensions.contains_key("code"));
}

#[tokio::test]
async fn it_fails_on_no_query() {
    let expected_error = "Must provide query string.";

    let router_service = from_supergraph_mock_callback(move |_req| unreachable!()).await;

    let request = SupergraphRequest::fake_builder()
        .build()
        .expect("expecting valid request")
        .try_into()
        .unwrap();

    let response = router_service
        .oneshot(request)
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await
        .next()
        .await
        .unwrap()
        .unwrap();
    let actual_error = response.errors[0].message.clone();
    assert_eq!(expected_error, actual_error);
    assert!(response.errors[0].extensions.contains_key("code"));
}

#[tokio::test]
async fn test_http_max_request_bytes() {
    /// Size of the JSON serialization of the request created by `fn canned_new`
    /// in `apollo-router/src/services/supergraph.rs`
    const CANNED_REQUEST_LEN: usize = 391;

    async fn with_config(http_max_request_bytes: usize) -> router::Response {
        let http_request = supergraph::Request::canned_builder()
            .build()
            .unwrap()
            .supergraph_request
            .map(|body| {
                let json_bytes = serde_json::to_vec(&body).unwrap();
                assert_eq!(
                    json_bytes.len(),
                    CANNED_REQUEST_LEN,
                    "The request generated by `fn canned_new` \
                     in `apollo-router/src/services/supergraph.rs` has changed. \
                     Please update `CANNED_REQUEST_LEN` accordingly."
                );
                hyper::Body::from(json_bytes)
            });
        let config = serde_json::json!({
            "limits": {
                "http_max_request_bytes": http_max_request_bytes
            }
        });
        crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap()
            .oneshot(router::Request::from(http_request))
            .await
            .unwrap()
    }
    // Send a request just at (under) the limit
    let response = with_config(CANNED_REQUEST_LEN).await.response;
    assert_eq!(response.status(), http::StatusCode::OK);

    // Send a request just over the limit
    let response = with_config(CANNED_REQUEST_LEN - 1).await.response;
    assert_eq!(response.status(), http::StatusCode::PAYLOAD_TOO_LARGE);
}

//  Test query batching

#[tokio::test]
async fn it_only_accepts_batch_http_link_mode_for_query_batch() {
    let expected_response: serde_json::Value = serde_json::from_str(include_str!(
        "../query_batching/testdata/batching_not_enabled_response.json"
    ))
    .unwrap();

    async fn with_config() -> router::Response {
        let http_request = supergraph::Request::canned_builder()
            .build()
            .unwrap()
            .supergraph_request
            .map(|req: crate::request::Request| {
                // Modify the request so that it is a valid array of requests.
                let mut json_bytes = serde_json::to_vec(&req).unwrap();
                let mut result = vec![b'['];
                result.append(&mut json_bytes.clone());
                result.push(b',');
                result.append(&mut json_bytes);
                result.push(b']');
                hyper::Body::from(result)
            });
        let config = serde_json::json!({});
        crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap()
            .oneshot(router::Request::from(http_request))
            .await
            .unwrap()
    }
    // Send a request
    let response = with_config().await.response;
    assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
    let data: serde_json::Value =
        serde_json::from_slice(&hyper::body::to_bytes(response.into_body()).await.unwrap())
            .unwrap();
    assert_eq!(expected_response, data);
}

#[tokio::test]
async fn it_processes_a_valid_query_batch() {
    let expected_response: serde_json::Value = serde_json::from_str(include_str!(
        "../query_batching/testdata/expected_good_response.json"
    ))
    .unwrap();

    async fn with_config() -> router::Response {
        let http_request = supergraph::Request::canned_builder()
            .build()
            .unwrap()
            .supergraph_request
            .map(|req_2: crate::request::Request| {
                // Create clones of our standard query and update it to have 3 unique queries
                let mut req_1 = req_2.clone();
                let mut req_3 = req_2.clone();
                req_1.query = req_2.query.clone().map(|x| x.replace("upc\n", ""));
                req_3.query = req_2.query.clone().map(|x| x.replace("id name", "name"));

                // Modify the request so that it is a valid array of 3 requests.
                let mut json_bytes_1 = serde_json::to_vec(&req_1).unwrap();
                let mut json_bytes_2 = serde_json::to_vec(&req_2).unwrap();
                let mut json_bytes_3 = serde_json::to_vec(&req_3).unwrap();
                let mut result = vec![b'['];
                result.append(&mut json_bytes_1);
                result.push(b',');
                result.append(&mut json_bytes_2);
                result.push(b',');
                result.append(&mut json_bytes_3);
                result.push(b']');
                hyper::Body::from(result)
            });
        let config = serde_json::json!({
            "batching": {
                "enabled": true,
                "mode" : "batch_http_link"
            }
        });
        crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap()
            .oneshot(router::Request::from(http_request))
            .await
            .unwrap()
    }
    // Send a request
    let response = with_config().await.response;
    assert_eq!(response.status(), http::StatusCode::OK);
    let data: serde_json::Value =
        serde_json::from_slice(&hyper::body::to_bytes(response.into_body()).await.unwrap())
            .unwrap();
    assert_eq!(expected_response, data);
}

#[tokio::test]
async fn it_will_not_process_a_query_batch_without_enablement() {
    let expected_response: serde_json::Value = serde_json::from_str(include_str!(
        "../query_batching/testdata/batching_not_enabled_response.json"
    ))
    .unwrap();

    async fn with_config() -> router::Response {
        let http_request = supergraph::Request::canned_builder()
            .build()
            .unwrap()
            .supergraph_request
            .map(|req: crate::request::Request| {
                // Modify the request so that it is a valid array of requests.
                let mut json_bytes = serde_json::to_vec(&req).unwrap();
                let mut result = vec![b'['];
                result.append(&mut json_bytes.clone());
                result.push(b',');
                result.append(&mut json_bytes);
                result.push(b']');
                hyper::Body::from(result)
            });
        let config = serde_json::json!({});
        crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap()
            .oneshot(router::Request::from(http_request))
            .await
            .unwrap()
    }
    // Send a request
    let response = with_config().await.response;
    assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
    let data: serde_json::Value =
        serde_json::from_slice(&hyper::body::to_bytes(response.into_body()).await.unwrap())
            .unwrap();
    assert_eq!(expected_response, data);
}

#[tokio::test]
async fn it_will_not_process_a_poorly_formatted_query_batch() {
    let expected_response: serde_json::Value = serde_json::from_str(include_str!(
        "../query_batching/testdata/badly_formatted_batch_response.json"
    ))
    .unwrap();

    async fn with_config() -> router::Response {
        let http_request = supergraph::Request::canned_builder()
            .build()
            .unwrap()
            .supergraph_request
            .map(|req: crate::request::Request| {
                // Modify the request so that it is a valid array of requests.
                let mut json_bytes = serde_json::to_vec(&req).unwrap();
                let mut result = vec![b'['];
                result.append(&mut json_bytes.clone());
                result.push(b',');
                result.append(&mut json_bytes);
                // Deliberately omit the required trailing ]
                hyper::Body::from(result)
            });
        let config = serde_json::json!({
            "batching": {
                "enabled": true,
                "mode" : "batch_http_link"
            }
        });
        crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap()
            .oneshot(router::Request::from(http_request))
            .await
            .unwrap()
    }
    // Send a request
    let response = with_config().await.response;
    assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
    let data: serde_json::Value =
        serde_json::from_slice(&hyper::body::to_bytes(response.into_body()).await.unwrap())
            .unwrap();
    assert_eq!(expected_response, data);
}

#[tokio::test]
async fn it_will_process_a_non_batched_defered_query() {
    let expected_response = "\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{\"data\":{\"topProducts\":[{\"upc\":\"1\",\"name\":\"Table\",\"reviews\":[{\"product\":{\"name\":\"Table\"},\"author\":{\"id\":\"1\",\"name\":\"Ada Lovelace\"}},{\"product\":{\"name\":\"Table\"},\"author\":{\"id\":\"2\",\"name\":\"Alan Turing\"}}]},{\"upc\":\"2\",\"name\":\"Couch\",\"reviews\":[{\"product\":{\"name\":\"Couch\"},\"author\":{\"id\":\"1\",\"name\":\"Ada Lovelace\"}}]}]},\"hasNext\":true}\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{\"hasNext\":false,\"incremental\":[{\"data\":{\"id\":\"1\"},\"path\":[\"topProducts\",0,\"reviews\",0]},{\"data\":{\"id\":\"4\"},\"path\":[\"topProducts\",0,\"reviews\",1]},{\"data\":{\"id\":\"2\"},\"path\":[\"topProducts\",1,\"reviews\",0]}]}\r\n--graphql--\r\n";
    async fn with_config() -> router::Response {
        let query = "
            query TopProducts($first: Int) {
                topProducts(first: $first) {
                    upc
                    name
                    reviews {
                        ... @defer {
                        id
                        }
                        product { name }
                        author { id name }
                    }
                }
            }
        ";
        let http_request = supergraph::Request::canned_builder()
            .header(http::header::ACCEPT, MULTIPART_DEFER_CONTENT_TYPE)
            .query(query)
            .build()
            .unwrap()
            .supergraph_request
            .map(|req: crate::request::Request| {
                let bytes = serde_json::to_vec(&req).unwrap();
                hyper::Body::from(bytes)
            });
        let config = serde_json::json!({
            "batching": {
                "enabled": true,
                "mode" : "batch_http_link"
            }
        });
        crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap()
            .oneshot(router::Request::from(http_request))
            .await
            .unwrap()
    }
    // Send a request
    let response = with_config().await.response;
    assert_eq!(response.status(), http::StatusCode::OK);
    let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
    let data = String::from_utf8_lossy(&bytes);
    assert_eq!(expected_response, data);
}

#[tokio::test]
async fn it_will_not_process_a_batched_deferred_query() {
    let expected_response = "[\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{\"errors\":[{\"message\":\"Deferred responses and subscriptions aren't supported in batches\",\"extensions\":{\"code\":\"BATCHING_DEFER_UNSUPPORTED\"}}]}\r\n--graphql--\r\n, \r\n--graphql\r\ncontent-type: application/json\r\n\r\n{\"errors\":[{\"message\":\"Deferred responses and subscriptions aren't supported in batches\",\"extensions\":{\"code\":\"BATCHING_DEFER_UNSUPPORTED\"}}]}\r\n--graphql--\r\n]";

    async fn with_config() -> router::Response {
        let query = "
            query TopProducts($first: Int) {
                topProducts(first: $first) {
                    upc
                    name
                    reviews {
                        ... @defer {
                        id
                        }
                        product { name }
                        author { id name }
                    }
                }
            }
        ";
        let http_request = supergraph::Request::canned_builder()
            .header(http::header::ACCEPT, MULTIPART_DEFER_CONTENT_TYPE)
            .query(query)
            .build()
            .unwrap()
            .supergraph_request
            .map(|req: crate::request::Request| {
                // Modify the request so that it is a valid array of requests.
                let mut json_bytes = serde_json::to_vec(&req).unwrap();
                let mut result = vec![b'['];
                result.append(&mut json_bytes.clone());
                result.push(b',');
                result.append(&mut json_bytes);
                result.push(b']');
                hyper::Body::from(result)
            });
        let config = serde_json::json!({
            "batching": {
                "enabled": true,
                "mode" : "batch_http_link"
            }
        });
        crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap()
            .oneshot(router::Request::from(http_request))
            .await
            .unwrap()
    }
    // Send a request
    let response = with_config().await.response;
    assert_eq!(response.status(), http::StatusCode::NOT_ACCEPTABLE);
    let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
    let data = String::from_utf8_lossy(&bytes);
    assert_eq!(expected_response, data);
}

/// <https://github.com/apollographql/router/issues/3541>
#[tokio::test]
async fn escaped_quotes_in_string_literal() {
    let query = r#"
        query TopProducts($first: Int) {
            topProducts(first: $first) {
                name
                reviewsForAuthor(authorID: "\"1\"") {
                    body
                }
            }
        }
    "#;
    let request = supergraph::Request::fake_builder()
        .query(query)
        .variable("first", 2)
        .build()
        .unwrap();
    let config = serde_json::json!({
        "include_subgraph_errors": {"all": true},
    });
    let subgraph_query_log = Arc::new(Mutex::new(Vec::new()));
    let subgraph_query_log_2 = subgraph_query_log.clone();
    let mut response = crate::TestHarness::builder()
        .configuration_json(config)
        .unwrap()
        .subgraph_hook(move |subgraph_name, service| {
            let is_reviews = subgraph_name == "reviews";
            let subgraph_name = subgraph_name.to_owned();
            let subgraph_query_log_3 = subgraph_query_log_2.clone();
            service
                .map_request(move |request: subgraph::Request| {
                    subgraph_query_log_3.lock().unwrap().push((
                        subgraph_name.clone(),
                        request.subgraph_request.body().query.clone(),
                    ));
                    request
                })
                .map_response(move |mut response| {
                    if is_reviews {
                        // Replace "couldn't find mock for query" error with empty data
                        let graphql_response = response.response.body_mut();
                        graphql_response.errors.clear();
                        graphql_response.data = Some(serde_json_bytes::json!({
                            "_entities": {"reviews": []},
                        }));
                    }
                    response
                })
                .boxed()
        })
        .build_supergraph()
        .await
        .unwrap()
        .oneshot(request)
        .await
        .unwrap();
    let graphql_response = response.next_response().await.unwrap();
    let subgraph_query_log = subgraph_query_log.lock().unwrap();
    insta::assert_debug_snapshot!((graphql_response, &subgraph_query_log));
    let subgraph_query = subgraph_query_log[1].1.as_ref().unwrap();

    // The string literal made it through unchanged:
    assert!(subgraph_query.contains(r#"reviewsForAuthor(authorID:"\"1\"")"#));
}
