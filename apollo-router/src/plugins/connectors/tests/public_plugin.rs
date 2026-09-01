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
use apollo_federation::connectors::runtime::http_json_transport::TransportResponse;
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

/// Asserts that a plugin can read and rewrite a *successful* connector
/// [`request_service::Response`] through its public accessors:
/// [`Response::data`]/[`Response::set_data`] (the hit branch), the false-return
/// no-op branch of [`Response::set_error_message`]/[`Response::set_error_code`],
/// the newly-public `transport_result` field (read *and* written), and the
/// `context` read-write path shared between [`request_service::Request`] and
/// [`request_service::Response`].
///
/// [`Response::data`]: request_service::Response::data
/// [`Response::set_data`]: request_service::Response::set_data
/// [`Response::set_error_message`]: request_service::Response::set_error_message
/// [`Response::set_error_code`]: request_service::Response::set_error_code
#[tokio::test]
async fn public_unstable_plugin_can_wrap_a_connector_response_with_data() {
    let mock_server = MockServer::start().await;
    mock_api::user_1().mount(&mock_server).await;

    /// What the plugin observed on the response side, read out after the
    /// router has finished — the wrapped service runs inside a buffered
    /// worker task, so assertions belong out here rather than inline.
    #[derive(Debug, Default)]
    struct Observed {
        /// Read back from `Response::context`: proves it's the same shared
        /// context that `Request::context` was written to, on the request side.
        context_value_written_on_request: Option<String>,
        /// Written directly into `Response::context`, then read back immediately.
        context_value_written_on_response: Option<String>,
        /// The status recorded on the (`Ok`) `Response::transport_result`, before
        /// the plugin rewrites it.
        transport_status_before: Option<u16>,
        /// `transport_result`'s status and a header the plugin adds to it,
        /// read back immediately after the rewrite.
        transport_status_after: Option<u16>,
        transport_header_after: Option<String>,
        data_before: Option<serde_json_bytes::Value>,
        error_before_is_some: bool,
        set_error_message_result: bool,
        set_error_code_result: bool,
        error_after_noop_setters_is_some: bool,
        set_data_result: bool,
        data_after: Option<serde_json_bytes::Value>,
    }

    #[derive(Clone)]
    struct ResponseWrappingPlugin {
        observed: Arc<Mutex<Observed>>,
    }

    #[async_trait::async_trait]
    impl PluginUnstable for ResponseWrappingPlugin {
        type Config = Conf;

        async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
            unreachable!("added via TestHarness::extra_unstable_plugin")
        }

        fn connector_request_service(
            &self,
            service: request_service::BoxService,
            _source_name: String,
        ) -> request_service::BoxService {
            let observed = self.observed.clone();
            BoxService::new(
                service
                    .map_request(|request: request_service::Request| {
                        request
                            .context
                            .insert("from-request", String::from("hello-from-request"))
                            .unwrap();
                        request
                    })
                    .map_response(move |mut response: request_service::Response| {
                        let mut observed = observed.lock().unwrap();

                        observed.context_value_written_on_request =
                            response.context.get::<_, String>("from-request").unwrap();
                        response
                            .context
                            .insert("from-response", String::from("hello-from-response"))
                            .unwrap();
                        observed.context_value_written_on_response =
                            response.context.get::<_, String>("from-response").unwrap();

                        observed.transport_status_before = match &response.transport_result {
                            Ok(TransportResponse::Http(http_response)) => {
                                Some(http_response.inner.status.as_u16())
                            }
                            _ => None,
                        };

                        // `transport_result` is writable: rewrite the status and
                        // add a header to the raw transport outcome.
                        if let Ok(TransportResponse::Http(http_response)) =
                            &mut response.transport_result
                        {
                            http_response.inner.status = http::StatusCode::IM_A_TEAPOT;
                            http_response.inner.headers.insert(
                                http::HeaderName::from_static("x-rewritten-by-plugin"),
                                http::HeaderValue::from_static("yes"),
                            );
                        }
                        if let Ok(TransportResponse::Http(http_response)) =
                            &response.transport_result
                        {
                            observed.transport_status_after =
                                Some(http_response.inner.status.as_u16());
                            observed.transport_header_after = http_response
                                .inner
                                .headers
                                .get("x-rewritten-by-plugin")
                                .and_then(|v| v.to_str().ok())
                                .map(str::to_string);
                        }

                        observed.data_before = response.data().cloned();
                        observed.error_before_is_some = response.error().is_some();

                        // No-op branches: a success can't be turned into a
                        // failure through the error setters.
                        observed.set_error_message_result =
                            response.set_error_message("should not apply");
                        observed.set_error_code_result =
                            response.set_error_code("SHOULD_NOT_APPLY");
                        observed.error_after_noop_setters_is_some = response.error().is_some();

                        // Hit branch: `set_data` replaces the mapped data.
                        observed.set_data_result = response.set_data(serde_json_bytes::json!({
                            "id": 1,
                            "name": "Rewritten By Plugin",
                            "username": "Bret",
                        }));
                        observed.data_after = response.data().cloned();

                        drop(observed);
                        response
                    }),
            )
        }

        fn unstable_method(&self) {}
    }

    let observed = Arc::new(Mutex::new(Observed::default()));
    let plugin = ResponseWrappingPlugin {
        observed: observed.clone(),
    };

    let response = execute_with_unstable_plugin(
        STEEL_THREAD_SCHEMA,
        &mock_server.uri(),
        "query { me { id name username } }",
        plugin,
    )
    .await;

    // The rewrite made through `set_data` reached the client.
    assert_eq!(
        response,
        serde_json::json!({
            "data": { "me": { "id": 1, "name": "Rewritten By Plugin", "username": "Bret" } }
        })
    );

    let observed = std::mem::take(&mut *observed.lock().unwrap());

    // `Request::context` and `Response::context` are the same shared context.
    assert_eq!(
        observed.context_value_written_on_request,
        Some("hello-from-request".to_string())
    );
    // `Response::context` is itself readable and writable.
    assert_eq!(
        observed.context_value_written_on_response,
        Some("hello-from-response".to_string())
    );

    // `Response::transport_result` reports the raw transport outcome...
    assert_eq!(observed.transport_status_before, Some(200));
    // ...and is itself writable: the rewritten status and header stuck.
    assert_eq!(observed.transport_status_after, Some(418));
    assert_eq!(observed.transport_header_after, Some("yes".to_string()));

    // Hit branches: this is a data response.
    assert!(observed.data_before.is_some());
    assert!(!observed.error_before_is_some);

    // No-op branches: the error setters can't turn a success into a failure.
    assert!(!observed.set_error_message_result);
    assert!(!observed.set_error_code_result);
    assert!(!observed.error_after_noop_setters_is_some);

    // Hit branch: `set_data` actually applied.
    assert!(observed.set_data_result);
    assert_eq!(
        observed.data_after,
        Some(serde_json_bytes::json!({
            "id": 1,
            "name": "Rewritten By Plugin",
            "username": "Bret",
        }))
    );
}

/// Asserts that a plugin can read and rewrite a *failed* connector
/// [`request_service::Response`] through its public accessors: the false-return
/// no-op branch of [`Response::set_data`], the hit branch of
/// [`Response::error`]/[`Response::set_error_message`]/[`Response::set_error_code`],
/// and `transport_result` (read *and* written) for a call whose transport
/// succeeded (a real HTTP 404) even though the mapped response is an error.
/// Also asserts the documented independence of the two: rewriting
/// `transport_result`'s status does not change the `http.status` already baked
/// into the client-visible error's extensions, since the mapped response is not
/// recomputed from it.
///
/// [`Response::set_data`]: request_service::Response::set_data
/// [`Response::error`]: request_service::Response::error
/// [`Response::set_error_message`]: request_service::Response::set_error_message
/// [`Response::set_error_code`]: request_service::Response::set_error_code
#[tokio::test]
async fn public_unstable_plugin_can_wrap_a_connector_response_with_error() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(serde_json::json!({"error": "not found"})),
        )
        .mount(&mock_server)
        .await;

    #[derive(Debug, Default)]
    struct Observed {
        transport_status_before: Option<u16>,
        transport_status_after: Option<u16>,
        data_before_is_some: bool,
        error_before: Option<(String, String)>,
        set_data_result: bool,
        data_after_noop_is_some: bool,
        set_error_message_result: bool,
        set_error_code_result: bool,
        error_after: Option<(String, String)>,
    }

    #[derive(Clone)]
    struct ResponseWrappingPlugin {
        observed: Arc<Mutex<Observed>>,
    }

    #[async_trait::async_trait]
    impl PluginUnstable for ResponseWrappingPlugin {
        type Config = Conf;

        async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
            unreachable!("added via TestHarness::extra_unstable_plugin")
        }

        fn connector_request_service(
            &self,
            service: request_service::BoxService,
            _source_name: String,
        ) -> request_service::BoxService {
            let observed = self.observed.clone();
            BoxService::new(
                service.map_response(move |mut response: request_service::Response| {
                    let mut observed = observed.lock().unwrap();

                    observed.transport_status_before = match &response.transport_result {
                        Ok(TransportResponse::Http(http_response)) => {
                            Some(http_response.inner.status.as_u16())
                        }
                        _ => None,
                    };

                    // `transport_result` is writable here too. This does *not*
                    // change the client-visible error, which was already mapped
                    // from the original (404) status.
                    if let Ok(TransportResponse::Http(http_response)) =
                        &mut response.transport_result
                    {
                        http_response.inner.status = http::StatusCode::IM_A_TEAPOT;
                    }
                    observed.transport_status_after = match &response.transport_result {
                        Ok(TransportResponse::Http(http_response)) => {
                            Some(http_response.inner.status.as_u16())
                        }
                        _ => None,
                    };

                    observed.data_before_is_some = response.data().is_some();
                    observed.error_before = response
                        .error()
                        .map(|error| (error.message.clone(), error.code().to_string()));

                    // No-op branch: a failure can't be turned into data here.
                    observed.set_data_result =
                        response.set_data(serde_json_bytes::json!({ "nope": true }));
                    observed.data_after_noop_is_some = response.data().is_some();

                    // Hit branches: rewrite the error returned to the client.
                    observed.set_error_message_result =
                        response.set_error_message("rewritten by plugin");
                    observed.set_error_code_result = response.set_error_code("REWRITTEN_CODE");
                    observed.error_after = response
                        .error()
                        .map(|error| (error.message.clone(), error.code().to_string()));

                    drop(observed);
                    response
                }),
            )
        }

        fn unstable_method(&self) {}
    }

    let observed = Arc::new(Mutex::new(Observed::default()));
    let plugin = ResponseWrappingPlugin {
        observed: observed.clone(),
    };

    let response = execute_with_unstable_plugin(
        STEEL_THREAD_SCHEMA,
        &mock_server.uri(),
        "query { me { id name username } }",
        plugin,
    )
    .await;

    // The plugin's rewrite of the error reached the client.
    let errors = response
        .get("errors")
        .and_then(|e| e.as_array())
        .expect("expected the failed connector request to produce an error");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["message"], "rewritten by plugin");
    assert_eq!(errors[0]["extensions"]["code"], "REWRITTEN_CODE");
    // The client-visible error still reports the *original* transport status:
    // rewriting `transport_result` after the fact doesn't reach it, because the
    // mapped response was already built from the real 404.
    assert_eq!(errors[0]["extensions"]["http"]["status"], 404);

    let observed = std::mem::take(&mut *observed.lock().unwrap());

    // The transport itself succeeded (a real HTTP 404 came back); only the
    // *mapped* response is an error.
    assert_eq!(observed.transport_status_before, Some(404));
    // `transport_result` is writable: the rewritten status stuck...
    assert_eq!(observed.transport_status_after, Some(418));
    // ...independently of the mapped response asserted above.

    // Hit branches: this is an error response.
    assert!(!observed.data_before_is_some);
    assert!(observed.error_before.is_some());

    // No-op branch: `set_data` can't turn a failure into a success.
    assert!(!observed.set_data_result);
    assert!(!observed.data_after_noop_is_some);

    // Hit branches: the error setters actually applied.
    assert!(observed.set_error_message_result);
    assert!(observed.set_error_code_result);
    assert_eq!(
        observed.error_after,
        Some((
            "rewritten by plugin".to_string(),
            "REWRITTEN_CODE".to_string()
        ))
    );
}
