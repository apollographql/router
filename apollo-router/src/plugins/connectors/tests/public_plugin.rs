//! Tests that `connector_request_service` is reachable from the *public* plugin
//! interface, i.e. from an out-of-tree plugin implementing [`PluginUnstable`].
//!
//! Every other test of this hook implements [`PluginPrivate`] directly, which does
//! not exercise the `DynPlugin` -> `PluginUnstable` forwarding in `plugin/mod.rs`.
//! That forwarding is the entire feature, so it gets its own coverage here.
//!
//! [`PluginPrivate`]: crate::plugin::PluginPrivate

use std::sync::Arc;
use std::sync::Mutex;

use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceExt;
use tower::util::BoxService;

use super::*;
use crate::plugin::PluginInit;
use crate::plugin::PluginUnstable;
use crate::services::connector::request_service;

/// What the plugin under test observed, so assertions can be made on the router's
/// side of the wire rather than only on what the mock API received.
#[derive(Debug, Default)]
struct Observed {
    /// One entry per `connector_request_service` call, i.e. per source the router
    /// built a service for.
    source_names: Vec<String>,
    /// One entry per connector request that actually flowed through the wrapped
    /// service.
    request_uris: Vec<String>,
    /// The operation body read back off `Request::supergraph_request`.
    supergraph_queries: Vec<String>,
}

#[derive(Clone)]
struct ObservingPlugin {
    observed: Arc<Mutex<Observed>>,
}

#[derive(Deserialize, JsonSchema)]
struct Conf {}

#[async_trait::async_trait]
impl PluginUnstable for ObservingPlugin {
    type Config = Conf;

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!("added via TestHarness::extra_unstable_plugin, never constructed from config")
    }

    fn connector_request_service(
        &self,
        service: request_service::BoxService,
        source_name: String,
    ) -> request_service::BoxService {
        self.observed
            .lock()
            .unwrap()
            .source_names
            .push(source_name.clone());

        let observed = self.observed.clone();
        BoxService::new(
            service.map_request(move |mut request: request_service::Request| {
                // Read the originating operation through the public accessor.
                let query = request.supergraph_request().body().query.clone();

                // Read the outbound HTTP request, and rewrite it, through the
                // public `transport_request` field.
                let TransportRequest::Http(http_request) = &mut request.transport_request else {
                    return request;
                };
                let mut observed = observed.lock().unwrap();
                observed
                    .request_uris
                    .push(http_request.inner.uri().to_string());
                if let Some(query) = query {
                    observed.supergraph_queries.push(query);
                }
                http_request
                    .inner
                    .headers_mut()
                    .insert("x-from-plugin", "yes".parse().unwrap());
                drop(observed);
                request
            }),
        )
    }

    fn unstable_method(&self) {}
}

/// Registers a plugin through the public `PluginUnstable` interface and asserts the
/// connector hook actually runs: the plugin sees the source name and the outbound
/// request, and its rewrite reaches the upstream API.
#[tokio::test]
async fn public_unstable_plugin_wraps_connector_request_service() {
    let mock_server = MockServer::start().await;
    mock_api::user_1().mount(&mock_server).await;

    let observed = Arc::new(Mutex::new(Observed::default()));
    let plugin = ObservingPlugin {
        observed: observed.clone(),
    };

    let query = "query { me { id name username } }";
    let response =
        execute_with_unstable_plugin(STEEL_THREAD_SCHEMA, &mock_server.uri(), query, plugin).await;

    // The request still succeeded end to end.
    assert_eq!(
        response,
        serde_json::json!({
            "data": { "me": { "id": 1, "name": "Leanne Graham", "username": "Bret" } }
        })
    );

    // The router's side of the wire: the hook ran, with the source name, and the
    // plugin could read both the outbound request and the originating operation.
    // Taken by value so the guard is not held across the await below.
    let observed = std::mem::take(&mut *observed.lock().unwrap());
    assert_eq!(
        observed.source_names,
        vec!["connectors.json".to_string()],
        "connector_request_service should be called once per connector source"
    );
    assert_eq!(
        observed.request_uris.len(),
        1,
        "the wrapped service should see exactly one connector request"
    );
    assert!(
        observed.request_uris[0].ends_with("/users/1"),
        "unexpected connector URI: {}",
        observed.request_uris[0]
    );
    assert_eq!(observed.supergraph_queries, vec![query.to_string()]);

    // The upstream's side of the wire: the plugin's rewrite actually went out.
    req_asserts::matches(
        &mock_server.received_requests().await.unwrap(),
        vec![Matcher::new().method("GET").path("/users/1").header(
            HeaderName::from_static("x-from-plugin"),
            HeaderValue::from_static("yes"),
        )],
    );
}

/// Asserts that a plugin can fail a connector request without making it, using
/// [`request_service::Request::into_error_response`] — the in-process equivalent of
/// a coprocessor returning `Control::Break` from the `ConnectorRequest` stage.
#[tokio::test]
async fn public_unstable_plugin_can_break_a_connector_request() {
    let mock_server = MockServer::start().await;
    mock_api::user_1().mount(&mock_server).await;

    #[derive(Clone)]
    struct BreakingPlugin;

    #[async_trait::async_trait]
    impl PluginUnstable for BreakingPlugin {
        type Config = Conf;

        async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
            unreachable!("added via TestHarness::extra_unstable_plugin")
        }

        fn connector_request_service(
            &self,
            service: request_service::BoxService,
            _source_name: String,
        ) -> request_service::BoxService {
            BoxService::new(tower::service_fn(
                move |request: request_service::Request| {
                    // `service` is intentionally dropped: the upstream call is never made.
                    let _ = &service;
                    async move {
                        Ok(request.into_error_response(
                            "upstream is unhealthy",
                            "CIRCUIT_OPEN",
                            [("k1", "v1"), ("code", "dummy"), ("k2", "v2")],
                        ))
                    }
                },
            ))
        }

        fn unstable_method(&self) {}
    }

    let response = execute_with_unstable_plugin(
        STEEL_THREAD_SCHEMA,
        &mock_server.uri(),
        "query { me { id name username } }",
        BreakingPlugin,
    )
    .await;

    let errors = response
        .get("errors")
        .and_then(|e| e.as_array())
        .expect("expected the broken connector request to produce an error");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["message"], "upstream is unhealthy");
    assert_eq!(errors[0]["extensions"]["code"], "CIRCUIT_OPEN");
    assert_eq!(errors[0]["extensions"]["k1"], "v1");
    assert_eq!(errors[0]["extensions"]["k2"], "v2");

    // The plugin broke the request before it was made, so the upstream saw nothing.
    assert!(
        mock_server.received_requests().await.unwrap().is_empty(),
        "the connector request should never have reached the upstream"
    );
}
