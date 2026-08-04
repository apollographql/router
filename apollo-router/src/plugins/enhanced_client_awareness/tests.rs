use http::StatusCode;
use tower::ServiceExt;

use super::EnhancedClientAwareness;
use crate::Context;
use crate::json_ext::Object;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::enhanced_client_awareness::CLIENT_LIBRARY_KEY;
use crate::plugins::enhanced_client_awareness::CLIENT_LIBRARY_NAME_KEY;
use crate::plugins::enhanced_client_awareness::CLIENT_LIBRARY_VERSION_KEY;
use crate::plugins::enhanced_client_awareness::Config;
use crate::plugins::telemetry::CLIENT_LIBRARY_NAME;
use crate::plugins::telemetry::CLIENT_LIBRARY_VERSION;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::services::supergraph;

#[tokio::test]
async fn given_client_library_metadata_adds_values_to_context() {
    let (mock, mut handle) = tower_test::mock::pair::<SupergraphRequest, SupergraphResponse>();

    let driver = tokio::spawn(async move {
        let (request, responder) = handle.next_request().await.unwrap();
        assert!(
            request.context.contains_key(CLIENT_LIBRARY_NAME),
            "Missing CLIENT_LIBRARY_NAME key/value"
        );
        let client_library_name: String = request
            .context
            .get(CLIENT_LIBRARY_NAME)
            .unwrap_or_default()
            .unwrap_or_default();
        assert_eq!(client_library_name, "apollo-general-client-library");
        assert!(
            request.context.contains_key(CLIENT_LIBRARY_VERSION),
            "Missing CLIENT_LIBRARY_VERSION key/value"
        );
        let client_library_version: String = request
            .context
            .get(CLIENT_LIBRARY_VERSION)
            .unwrap_or_default()
            .unwrap_or_default();
        assert_eq!(client_library_version, "0.1.0");
        responder.send_response(SupergraphResponse::fake_builder().build().unwrap());
    });

    let mut clients_map = Object::new();
    clients_map.insert(CLIENT_LIBRARY_NAME_KEY, "apollo-general-client-library");
    clients_map.insert(CLIENT_LIBRARY_VERSION_KEY, "0.1.0");
    let mut extensions_map = Object::new();
    extensions_map.insert(CLIENT_LIBRARY_KEY, clients_map.into_value());

    EnhancedClientAwareness::new(PluginInit::fake_new(Config {}, Default::default()))
        .await
        .unwrap()
        .supergraph_service(mock.boxed_clone())
        .oneshot(
            supergraph::Request::fake_builder()
                .context(Context::default())
                .query("{query:{ foo { bar } }}")
                .extensions(extensions_map)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    crate::plugin::test::await_mock_driver(driver).await;
}

#[tokio::test]
async fn without_client_library_metadata_does_not_add_values_to_context() {
    let (mock, mut handle) = tower_test::mock::pair::<SupergraphRequest, SupergraphResponse>();

    let driver = tokio::spawn(async move {
        let (request, responder) = handle.next_request().await.unwrap();
        assert!(!request.context.contains_key(CLIENT_LIBRARY_NAME));
        assert!(!request.context.contains_key(CLIENT_LIBRARY_VERSION));
        responder.send_response(SupergraphResponse::fake_builder().build().unwrap());
    });

    EnhancedClientAwareness::new(PluginInit::fake_new(Config {}, Default::default()))
        .await
        .unwrap()
        .supergraph_service(mock.boxed_clone())
        .oneshot(
            supergraph::Request::fake_builder()
                .context(Context::default())
                .query("{query:{ foo { bar } }}")
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    crate::plugin::test::await_mock_driver(driver).await;
}

#[tokio::test]
async fn invalid_library_name_returns_bad_request() {
    let (mock, handle) = tower_test::mock::pair::<SupergraphRequest, SupergraphResponse>();

    let service_stack =
        EnhancedClientAwareness::new(PluginInit::fake_new(Config {}, Default::default()))
            .await
            .unwrap()
            .supergraph_service(mock.boxed_clone());

    let mut clients_map = Object::new();
    clients_map.insert(CLIENT_LIBRARY_NAME_KEY, r#"invalid";||"#);
    let mut extensions_map = Object::new();
    extensions_map.insert(CLIENT_LIBRARY_KEY, clients_map.into_value());

    let request = supergraph::Request::fake_builder()
        .context(Context::default())
        .query("{query:{ foo { bar } }}")
        .extensions(extensions_map)
        .build()
        .unwrap();

    let response = service_stack.oneshot(request).await.unwrap();
    assert_eq!(response.response.status(), StatusCode::BAD_REQUEST);
    crate::plugin::test::assert_no_mock_calls(handle).await;
}

#[tokio::test]
async fn invalid_library_version_returns_bad_request() {
    let (mock, handle) = tower_test::mock::pair::<SupergraphRequest, SupergraphResponse>();

    let service_stack =
        EnhancedClientAwareness::new(PluginInit::fake_new(Config {}, Default::default()))
            .await
            .unwrap()
            .supergraph_service(mock.boxed_clone());

    let mut clients_map = Object::new();
    clients_map.insert(CLIENT_LIBRARY_VERSION_KEY, r#"invalid";||"#);
    let mut extensions_map = Object::new();
    extensions_map.insert(CLIENT_LIBRARY_KEY, clients_map.into_value());

    let request = supergraph::Request::fake_builder()
        .context(Context::default())
        .query("{query:{ foo { bar } }}")
        .extensions(extensions_map)
        .build()
        .unwrap();

    let response = service_stack.oneshot(request).await.unwrap();
    assert_eq!(response.response.status(), StatusCode::BAD_REQUEST);
    crate::plugin::test::assert_no_mock_calls(handle).await;
}
