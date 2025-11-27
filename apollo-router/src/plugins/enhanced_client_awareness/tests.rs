use tower::ServiceExt;

use super::EnhancedClientAwareness;
use crate::Context;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugin::test::MockSupergraphService;
use crate::plugins::enhanced_client_awareness::CLIENT_APP_KEY;
use crate::plugins::enhanced_client_awareness::CLIENT_LIBRARY_KEY;
use crate::plugins::enhanced_client_awareness::CLIENT_NAME_KEY;
use crate::plugins::enhanced_client_awareness::CLIENT_VERSION_KEY;
use crate::plugins::enhanced_client_awareness::Config;
use crate::plugins::telemetry::CLIENT_LIBRARY_NAME;
use crate::plugins::telemetry::CLIENT_LIBRARY_VERSION;
use crate::plugins::telemetry::CLIENT_NAME;
use crate::plugins::telemetry::CLIENT_VERSION;
use crate::services::SupergraphResponse;
use crate::services::supergraph;

#[tokio::test]
async fn given_client_library_metadata_adds_values_to_context() {
    let mut mock_service = MockSupergraphService::new();

    mock_service.expect_call().returning(move |request| {
        // then
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

        SupergraphResponse::fake_builder().build()
    });

    let service_stack =
        EnhancedClientAwareness::new(PluginInit::fake_new(Config {}, Default::default()))
            .await
            .unwrap()
            .supergraph_service(mock_service.boxed());

    // given
    let mut clients_map = serde_json_bytes::map::Map::new();
    clients_map.insert(CLIENT_NAME_KEY, "apollo-general-client-library".into());
    clients_map.insert(CLIENT_VERSION_KEY, "0.1.0".into());
    let mut extensions_map = serde_json_bytes::map::Map::new();
    extensions_map.insert(CLIENT_LIBRARY_KEY, clients_map.into());

    // when
    let request = supergraph::Request::fake_builder()
        .context(Context::default())
        .query("{query:{ foo { bar } }}")
        .extensions(extensions_map)
        .build()
        .unwrap();

    let _ = service_stack.oneshot(request).await;
}

#[tokio::test]
async fn without_client_library_metadata_does_not_add_values_to_context() {
    let mut mock_service = MockSupergraphService::new();

    mock_service.expect_call().returning(move |request| {
        // then
        assert!(!request.context.contains_key(CLIENT_LIBRARY_NAME));
        assert!(!request.context.contains_key(CLIENT_LIBRARY_VERSION));

        SupergraphResponse::fake_builder().build()
    });

    let service_stack =
        EnhancedClientAwareness::new(PluginInit::fake_new(Config {}, Default::default()))
            .await
            .unwrap()
            .supergraph_service(mock_service.boxed());

    // when
    let request = supergraph::Request::fake_builder()
        .context(Context::default())
        .query("{query:{ foo { bar } }}")
        .build()
        .unwrap();

    let _ = service_stack.oneshot(request).await;
}

#[tokio::test]
async fn given_client_app_metadata_adds_values_to_context() {
    let mut mock_service = MockSupergraphService::new();

    mock_service.expect_call().returning(move |request| {
        // then
        assert!(
            request.context.contains_key(CLIENT_NAME),
            "Missing CLIENT_NAME key/value"
        );
        let client_name: String = request
            .context
            .get(CLIENT_NAME)
            .unwrap_or_default()
            .unwrap_or_default();
        assert_eq!(client_name, "apollo-general-client");

        assert!(
            request.context.contains_key(CLIENT_VERSION),
            "Missing CLIENT_VERSION key/value"
        );
        let client_version: String = request
            .context
            .get(CLIENT_VERSION)
            .unwrap_or_default()
            .unwrap_or_default();
        assert_eq!(client_version, "0.1.0");

        SupergraphResponse::fake_builder().build()
    });

    let service_stack =
        EnhancedClientAwareness::new(PluginInit::fake_new(Config {}, Default::default()))
            .await
            .unwrap()
            .supergraph_service(mock_service.boxed());

    // given
    let mut clients_map = serde_json_bytes::map::Map::new();
    clients_map.insert(CLIENT_NAME_KEY, "apollo-general-client".into());
    clients_map.insert(CLIENT_VERSION_KEY, "0.1.0".into());
    let mut extensions_map = serde_json_bytes::map::Map::new();
    extensions_map.insert(CLIENT_APP_KEY, clients_map.into());

    // when
    let request = supergraph::Request::fake_builder()
        .context(Context::default())
        .query("{query:{ foo { bar } }}")
        .extensions(extensions_map)
        .build()
        .unwrap();

    let _ = service_stack.oneshot(request).await;
}

#[tokio::test]
async fn without_client_app_metadata_does_not_add_values_to_context() {
    let mut mock_service = MockSupergraphService::new();

    mock_service.expect_call().returning(move |request| {
        // then
        assert!(!request.context.contains_key(CLIENT_NAME));
        assert!(!request.context.contains_key(CLIENT_VERSION));

        SupergraphResponse::fake_builder().build()
    });

    let service_stack =
        EnhancedClientAwareness::new(PluginInit::fake_new(Config {}, Default::default()))
            .await
            .unwrap()
            .supergraph_service(mock_service.boxed());

    // when
    let request = supergraph::Request::fake_builder()
        .context(Context::default())
        .query("{query:{ foo { bar } }}")
        .build()
        .unwrap();

    let _ = service_stack.oneshot(request).await;
}
