use http::Uri;
use mime::APPLICATION_JSON;
use serde_json_bytes::json;

use super::*;
use crate::services::supergraph;
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

    let mut router_service = super::from_supergraph_mock_callback(move |req| {
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
async fn test_experimental_http_max_request_bytes() {
    /// Size of the JSON serialization of the request created by `fn canned_new`
    /// in `apollo-router/src/services/supergraph.rs`
    const CANNED_REQUEST_LEN: usize = 391;

    async fn with_config(experimental_http_max_request_bytes: usize) -> router::Response {
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
            "preview_operation_limits": {
                "experimental_http_max_request_bytes": experimental_http_max_request_bytes
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
