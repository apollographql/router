use std::path::Path;
use std::time::Duration;

use apollo_router::Context;
use apollo_router::TestHarness;
use apollo_router::graphql;
use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInit;
use apollo_router::register_plugin;
use apollo_router::services::router;
use apollo_router::services::supergraph;
use async_trait::async_trait;
use axum::handler::HandlerWithoutStateExt;
use futures::FutureExt;
use http::HeaderValue;
use http::Method;
use http::Request as HttpRequest;
use http::header::ACCEPT;
use http::header::HeaderName;
use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt;
use wiremock::ResponseTemplate;

use crate::integration::IntegrationTest;
use crate::integration::common::graph_os_enabled;

const HAPPY_CONFIG: &str = include_str!("fixtures/happy.router.yaml");
const BROKEN_PLUGIN_CONFIG: &str = include_str!("fixtures/broken_plugin.router.yaml");
const INVALID_CONFIG: &str = "garbage: garbage";

#[tokio::test(flavor = "multi_thread")]
async fn test_happy() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(HAPPY_CONFIG)
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_invalid_config() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(INVALID_CONFIG)
        .build()
        .await;
    router.start().await;
    router.assert_not_started().await;
    router.assert_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_reload_config_valid() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(HAPPY_CONFIG)
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router.touch_config().await;
    router.assert_reloaded().await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_reload_config_with_broken_plugin() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(HAPPY_CONFIG)
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router.update_config(BROKEN_PLUGIN_CONFIG).await;
    router.assert_not_reloaded().await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_reload_config_with_broken_plugin_recovery() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(HAPPY_CONFIG)
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router.update_config(BROKEN_PLUGIN_CONFIG).await;
    router.assert_not_reloaded().await;
    router.execute_default_query().await;
    router.update_config(HAPPY_CONFIG).await;
    router.assert_reloaded().await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[cfg(target_family = "unix")]
async fn test_graceful_shutdown() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(HAPPY_CONFIG)
        .responder(ResponseTemplate::new(200).set_body_json(
            json!({"data":{"topProducts":[{"name":"Table"},{"name":"Couch"},{"name":"Chair"}]}}),
        ).set_delay(Duration::from_secs(2)))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Send a request in another thread, it'll take 2 seconds to respond, so we can shut down the router while it is in flight.
    let client_handle =
        tokio::task::spawn(router.execute_default_query().then(|(_, response)| async {
            serde_json::from_slice::<graphql::Response>(&response.bytes().await.unwrap()).unwrap()
        }));

    // Pause to ensure that the request is in flight.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    router.graceful_shutdown().await;

    // We've shut down the router, but we should have got the full response.
    let data = client_handle.await.unwrap();
    insta::assert_json_snapshot!(data);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_force_config_reload_via_chaos() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            "experimental_chaos:
                force_config_reload: 2s",
        )
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.assert_reloaded().await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_force_schema_reload_via_chaos() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            "experimental_chaos:
                force_schema_reload: 2s",
        )
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.assert_reloaded().await;
    router.graceful_shutdown().await;
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn test_reload_via_sighup() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(HAPPY_CONFIG)
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.send_sighup().await;
    router.assert_no_reload_necessary().await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_shutdown_with_idle_connection() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(HAPPY_CONFIG)
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    let _conn = std::net::TcpStream::connect(router.bind_address()).unwrap();
    router.execute_default_query().await;
    tokio::time::timeout(Duration::from_secs(1), router.graceful_shutdown())
        .await
        .unwrap();
    Ok(())
}

async fn command_output(command: &mut Command) -> String {
    let output = command.output().await.unwrap();
    let success = output.status.success();
    let exit_code = output.status.code();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    format!(
        "Success: {success:?}\n\
        Exit code: {exit_code:?}\n\
        stderr:\n\
        {stderr}\n\
        stdout:\n\
        {stdout}"
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cli_config_experimental() {
    insta::assert_snapshot!(
        command_output(
            Command::new(IntegrationTest::router_location())
                .arg("config")
                .arg("experimental")
                .env("RUST_BACKTRACE", "") // Avoid "RUST_BACKTRACE=full detected" log on CI
        )
        .await
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cli_config_preview() {
    insta::assert_snapshot!(
        command_output(
            Command::new(IntegrationTest::router_location())
                .arg("config")
                .arg("preview")
                .env("RUST_BACKTRACE", "") // Avoid "RUST_BACKTRACE=full detected" log on CI
        )
        .await
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_experimental_notice() {
    let mut router = IntegrationTest::builder()
        .config(
            "
            telemetry:
              exporters:
                tracing:
                  experimental_response_trace_id:
                    enabled: true
            ",
        )
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router
        .wait_for_log_message(
            "You're using some \\\"experimental\\\" features of the Apollo Router",
        )
        .await;
    router.graceful_shutdown().await;
}

const TEST_PLUGIN_ORDERING_CONTEXT_KEY: &str = "ordering-trace";

/// <https://github.com/apollographql/router/issues/3207>
#[tokio::test(flavor = "multi_thread")]
async fn test_plugin_ordering() {
    async fn coprocessor(mut json: axum::Json<serde_json::Value>) -> axum::Json<serde_json::Value> {
        let stage = json["stage"].as_str().unwrap().to_owned();
        json["context"]["entries"]
            .as_object_mut()
            .unwrap()
            .entry(TEST_PLUGIN_ORDERING_CONTEXT_KEY)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap()
            .push(format!("coprocessor {stage}").into());
        json
    }

    async fn spawn_coprocessor() -> (String, ShutdownOnDrop) {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let shutdown_on_drop = ShutdownOnDrop(Some(tx));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let coprocessor_url = format!("http://{}", listener.local_addr().unwrap());
        let server = axum::serve(listener, coprocessor.into_make_service());
        let server = server.with_graceful_shutdown(async {
            let _ = rx.await;
        });
        tokio::spawn(async move {
            if let Err(e) = server.await {
                eprintln!("coprocessor server error: {e}");
            }
        });
        (coprocessor_url, shutdown_on_drop)
    }

    struct ShutdownOnDrop(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for ShutdownOnDrop {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }

    let (coprocessor_url, _shutdown_on_drop) = spawn_coprocessor().await;

    let rhai_main = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("test_plugin_ordering.rhai");
    // Repeat to get more confidence it’s deterministic
    for _ in 0..10 {
        let mut service = TestHarness::builder()
            .configuration_json(json!({
                "plugins": {
                    "experimental.test_ordering_1": {},
                    "experimental.test_ordering_2": {},
                    "experimental.test_ordering_3": {},
                },
                "rhai": {
                    "main": rhai_main,
                },
                "coprocessor": {
                    "url": coprocessor_url,
                    "router_http": {
                        "request": { "context": "all" },
                        "response": { "context": "all" },
                    },
                    "router": {
                        "request": { "context": "all" },
                        "response": { "context": "all" },
                    }
                },
            }))
            .unwrap()
            .build_router()
            .await
            .unwrap();
        let request = supergraph::Request::canned_builder().build().unwrap();
        let mut response = service
            .ready()
            .await
            .unwrap()
            .call(request.try_into().unwrap())
            .await
            .unwrap();
        let body = response.next_response().await.unwrap().unwrap();
        let body = String::from_utf8_lossy(&body);
        assert!(!body.contains("error"), "{}", body);
        let trace: Vec<String> = response
            .context
            .get(TEST_PLUGIN_ORDERING_CONTEXT_KEY)
            .unwrap()
            .unwrap();
        // Execution order: RouterHttp (top-level) first, then Router pipeline. Plugin fold order
        // puts coprocessor RouterHttp after Rhai. See request-lifecycle docs for full path.
        // and before test_ordering plugins. Then router pipeline (coprocessor Router, etc.).
        assert_eq!(
            trace,
            [
                "router_http Rhai map_request",
                "coprocessor RouterHttpRequest",
                "router_http Rust test_ordering_1 map_request",
                "router_http Rust test_ordering_2 map_request",
                "router_http Rust test_ordering_3 map_request",
                "coprocessor RouterRequest",
                "router_service Rust test_ordering_1 map_request",
                "router_service Rust test_ordering_2 map_request",
                "router_service Rust test_ordering_3 map_request",
                "supergraph_service Rhai map_request",
                "supergraph_service Rust test_ordering_1 map_request",
                "supergraph_service Rust test_ordering_2 map_request",
                "supergraph_service Rust test_ordering_3 map_request",
                "supergraph_service Rust test_ordering_3 map_response",
                "supergraph_service Rust test_ordering_2 map_response",
                "supergraph_service Rust test_ordering_1 map_response",
                "supergraph_service Rhai map_response",
                "router_service Rust test_ordering_3 map_response",
                "router_service Rust test_ordering_2 map_response",
                "router_service Rust test_ordering_1 map_response",
                "coprocessor RouterResponse",
                "router_http Rust test_ordering_3 map_response",
                "router_http Rust test_ordering_2 map_response",
                "router_http Rust test_ordering_1 map_response",
                "coprocessor RouterHttpResponse",
                "router_http Rhai map_response",
            ]
        );
    }
}

/// Asserts that request/response modifications at RouterHttp are visible downstream and to the client.
#[tokio::test(flavor = "multi_thread")]
async fn test_router_http_request_response_modification() {
    const CONTEXT_KEY: &str = "router_http_request_modified";
    const RESPONSE_HEADER: &str = "x-router-http-response";

    let mut service = TestHarness::builder()
        .router_http_hook(|service| {
            ServiceBuilder::new()
                .map_request(|request: router::Request| {
                    request.context.insert(CONTEXT_KEY, true).unwrap();
                    request
                })
                .map_response(|mut response: router::Response| {
                    response.response.headers_mut().insert(
                        HeaderName::from_static(RESPONSE_HEADER),
                        HeaderValue::from_static("ok"),
                    );
                    response
                })
                .service(service)
                .boxed()
        })
        .router_hook(|service| {
            ServiceBuilder::new()
                .map_request(|request: router::Request| {
                    // Downstream visibility: router_service sees the context set by router_http
                    let seen = request
                        .context
                        .get::<&str, bool>(CONTEXT_KEY)
                        .ok()
                        .flatten()
                        .unwrap_or(false);
                    assert!(seen, "router_service must see context set by router_http");
                    request
                })
                .service(service)
                .boxed()
        })
        .configuration_json(json!({}))
        .unwrap()
        .build_router()
        .await
        .unwrap();

    let request = supergraph::Request::canned_builder().build().unwrap();
    let response = service
        .ready()
        .await
        .unwrap()
        .call(request.try_into().unwrap())
        .await
        .unwrap();

    // Client receives the response header set by router_http map_response
    let value = response
        .response
        .headers()
        .get(RESPONSE_HEADER)
        .expect("response must have header set by router_http");
    assert_eq!(value, "ok");
}

/// Asserts that Rhai router_http hook runs in isolation and can set context.
#[tokio::test(flavor = "multi_thread")]
async fn test_rhai_router_http_in_isolation() {
    let rhai_main = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("rhai_router_http_only.rhai");

    let mut service = TestHarness::builder()
        .configuration_json(json!({
            "rhai": { "main": rhai_main.to_string_lossy().to_string() },
        }))
        .unwrap()
        .build_router()
        .await
        .unwrap();

    let request = supergraph::Request::canned_builder().build().unwrap();
    let mut response = service
        .ready()
        .await
        .unwrap()
        .call(request.try_into().unwrap())
        .await
        .unwrap();

    let _body = response.next_response().await.unwrap().unwrap();
    let ran: bool = response
        .context
        .get::<&str, bool>("rhai_router_http_ran")
        .ok()
        .flatten()
        .unwrap_or(false);
    assert!(ran, "router_http Rhai hook must have run and set context");
}

/// Asserts that coprocessor RouterHttp stage can set context and response headers, visible downstream and to the client.
#[tokio::test(flavor = "multi_thread")]
async fn test_coprocessor_router_http_request_response_modification() {
    const CONTEXT_KEY: &str = "coprocessor_router_http_ran";
    const RESPONSE_HEADER: &str = "x-coprocessor-router-http";

    async fn coprocessor(mut json: axum::Json<serde_json::Value>) -> axum::Json<serde_json::Value> {
        let stage = json["stage"].as_str().unwrap_or("").to_string();
        if stage == "RouterHttpRequest" {
            json["context"]["entries"]
                .as_object_mut()
                .unwrap()
                .insert(CONTEXT_KEY.to_string(), json!(true));
        }
        if stage == "RouterHttpResponse" {
            json["headers"]
                .as_object_mut()
                .unwrap()
                .insert(RESPONSE_HEADER.to_string(), json!(["ok"]));
        }
        json
    }

    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let coprocessor_url = format!("http://{}", listener.local_addr().unwrap());
    let server = axum::serve(listener, coprocessor.into_make_service());
    let server = server.with_graceful_shutdown(async {
        let _ = rx.await;
    });
    tokio::spawn(async move {
        if let Err(e) = server.await {
            eprintln!("coprocessor server error: {e}");
        }
    });

    let mut service = TestHarness::builder()
        .configuration_json(json!({
            "coprocessor": {
                "url": coprocessor_url,
                "router_http": {
                    "request": { "context": "all" },
                    "response": { "context": "all", "headers": true },
                }
            }
        }))
        .unwrap()
        .build_router()
        .await
        .unwrap();

    let request = supergraph::Request::canned_builder().build().unwrap();
    let mut response = service
        .ready()
        .await
        .unwrap()
        .call(request.try_into().unwrap())
        .await
        .unwrap();

    let _body = response.next_response().await.unwrap().unwrap();
    let ran: bool = response
        .context
        .get::<&str, bool>(CONTEXT_KEY)
        .ok()
        .flatten()
        .unwrap_or(false);
    assert!(
        ran,
        "coprocessor RouterHttpRequest must have run and set context"
    );

    let value = response
        .response
        .headers()
        .get(RESPONSE_HEADER)
        .expect("response must have header set by coprocessor RouterHttpResponse");
    assert_eq!(value, "ok");
}

/// Asserts that static landing requests (GET + Accept: text/html) do not go through RouterHttp.
#[tokio::test(flavor = "multi_thread")]
async fn test_static_landing_does_not_go_through_router_http() {
    const ROUTER_HTTP_RAN_KEY: &str = "router_http_ran_for_request";

    let mut service = TestHarness::builder()
        .router_http_hook(|service| {
            ServiceBuilder::new()
                .map_request(|request: router::Request| {
                    let _ = request.context.insert(ROUTER_HTTP_RAN_KEY, true);
                    request
                })
                .service(service)
                .boxed()
        })
        .configuration_json(json!({
            "homepage": { "enabled": true }
        }))
        .unwrap()
        .build_router()
        .await
        .unwrap();

    let mut http_req = HttpRequest::builder()
        .method(Method::GET)
        .uri("http://example.com/")
        .header(ACCEPT, "text/html")
        .body(router::body::from_bytes(vec![]))
        .unwrap();
    http_req.extensions_mut().insert(Context::new());
    let static_landing_request = router::Request::from(http_req);

    let response = service
        .ready()
        .await
        .unwrap()
        .call(static_landing_request)
        .await
        .unwrap();

    let ran: bool = response
        .context
        .get::<&str, bool>(ROUTER_HTTP_RAN_KEY)
        .ok()
        .flatten()
        .unwrap_or(false);
    assert!(!ran, "router_http must not run for static landing requests");

    assert_eq!(response.response.status(), 200);
    assert_eq!(
        response.response.headers().get(http::header::CONTENT_TYPE),
        Some(&HeaderValue::from_static("text/html; charset=utf-8"))
    );
}

macro_rules! make_plugin {
    ($mod_name: ident, $str_name: expr) => {
        mod $mod_name {
            use super::*;

            #[derive(Deserialize, JsonSchema)]
            pub(super) struct Config {}

            /// Dummy plugin (for testing purposes only)
            pub(super) struct TestOrderingPlugin;

            register_plugin!("experimental", $str_name, TestOrderingPlugin);

            #[async_trait]
            impl Plugin for TestOrderingPlugin {
                type Config = Config;

                async fn new(_init: PluginInit<Self::Config>) -> Result<Self, BoxError>
                where
                    Self: Sized,
                {
                    Ok(Self)
                }

                fn router_http_service(&self, service: router::BoxService) -> router::BoxService {
                    ServiceBuilder::new()
                        .map_request(|request: router::Request| {
                            test_plugin_ordering_push_trace(
                                &request.context,
                                format!("router_http Rust {} map_request", $str_name),
                            );
                            request
                        })
                        .map_response(|response: router::Response| {
                            test_plugin_ordering_push_trace(
                                &response.context,
                                format!("router_http Rust {} map_response", $str_name),
                            );
                            response
                        })
                        .service(service)
                        .boxed()
                }

                fn router_service(&self, service: router::BoxService) -> router::BoxService {
                    ServiceBuilder::new()
                        .map_request(|request: router::Request| {
                            test_plugin_ordering_push_trace(
                                &request.context,
                                format!("router_service Rust {} map_request", $str_name),
                            );
                            request
                        })
                        .map_response(|response: router::Response| {
                            test_plugin_ordering_push_trace(
                                &response.context,
                                format!("router_service Rust {} map_response", $str_name),
                            );
                            response
                        })
                        .service(service)
                        .boxed()
                }

                fn supergraph_service(
                    &self,
                    service: supergraph::BoxService,
                ) -> supergraph::BoxService {
                    ServiceBuilder::new()
                        .map_request(|request: supergraph::Request| {
                            test_plugin_ordering_push_trace(
                                &request.context,
                                format!("supergraph_service Rust {} map_request", $str_name),
                            );
                            request
                        })
                        .map_response(|response: supergraph::Response| {
                            test_plugin_ordering_push_trace(
                                &response.context,
                                format!("supergraph_service Rust {} map_response", $str_name),
                            );
                            response
                        })
                        .service(service)
                        .boxed()
                }
            }
        }
    };
}

// Order in Rust source code does not matter
make_plugin!(test_ordering_2, "test_ordering_2");
make_plugin!(test_ordering_1, "test_ordering_1");
make_plugin!(test_ordering_3, "test_ordering_3");

fn test_plugin_ordering_push_trace(context: &Context, entry: String) {
    context
        .upsert(
            TEST_PLUGIN_ORDERING_CONTEXT_KEY,
            |mut trace: Vec<String>| {
                trace.push(entry);
                trace
            },
        )
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_multi_pipelines() {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return;
    }
    let mut router = IntegrationTest::builder()
        .config(include_str!("fixtures/prometheus.router.yaml"))
        .responder(ResponseTemplate::new(500).set_delay(Duration::from_secs(10)))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = router.execute_default_query();
    // Long running request 1
    let _h1 = tokio::task::spawn(query);
    router
        .update_config(include_str!("fixtures/prometheus_updated.router.yaml"))
        .await;

    router.assert_reloaded().await;
    // Long running request 2
    let query = router.execute_default_query();
    let _h2 = tokio::task::spawn(query);
    let metrics = router
        .get_metrics_response()
        .await
        .expect("metrics")
        .text()
        .await
        .expect("metrics");

    // There should be two instances of the pipeline metrics
    let pipelines = Regex::new(r#"(?m)^apollo_router_pipelines[{].+[}] 1"#).expect("regex");
    assert_eq!(pipelines.captures_iter(&metrics).count(), 2);

    // There should be at least two connections, one active and one terminating.
    // There may be more than one in each category because reqwest does connection pooling.
    let terminating =
        Regex::new(r#"(?m)^apollo_router_open_connections[{].+terminating.+[}]"#).expect("regex");
    assert!(terminating.captures_iter(&metrics).count() >= 1);
    let active =
        Regex::new(r#"(?m)^apollo_router_open_connections[{].+active.+[}]"#).expect("regex");
    assert!(active.captures_iter(&metrics).count() >= 1);
}

/// This test ensures that the router will not leave pipelines hanging around
/// It has early cancel set to true in the config so that when we look at the pipelines after connection
/// termination they are removed.
#[tokio::test(flavor = "multi_thread")]
async fn test_forced_connection_shutdown() {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return;
    }
    let mut router = IntegrationTest::builder()
        .config(include_str!(
            "fixtures/small_connection_shutdown_timeout.router.yaml"
        ))
        .responder(ResponseTemplate::new(500).set_delay(Duration::from_secs(10)))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = router.execute_default_query();
    // Long running request 1
    let _h1 = tokio::task::spawn(query);
    router
        .update_config(include_str!(
            "fixtures/small_connection_shutdown_timeout_updated.router.yaml"
        ))
        .await;

    router.assert_reloaded().await;
    // Long running request 2
    let query = router.execute_default_query();
    let _h2 = tokio::task::spawn(query);
    let metrics = router
        .get_metrics_response()
        .await
        .expect("metrics")
        .text()
        .await
        .expect("metrics");
    tokio::time::sleep(Duration::from_millis(100)).await;
    // There should be two instances of the pipeline metrics
    // There should be at least two connections, one active and one terminating.
    // There may be more than one in each category because reqwest does connection pooling.
    let terminating =
        Regex::new(r#"(?m)^apollo_router_open_connections[{].+terminating.+[}]"#).expect("regex");
    assert!(terminating.captures_iter(&metrics).count() >= 1);
    let active =
        Regex::new(r#"(?m)^apollo_router_open_connections[{].+active.+[}]"#).expect("regex");
    assert!(active.captures_iter(&metrics).count() >= 1);
}

/// Test that plugins receive their previous configuration during hot reload
/// Uses the telemetry plugin which logs whether it received previous config
#[tokio::test(flavor = "multi_thread")]
async fn test_previous_configuration_propagation() -> Result<(), BoxError> {
    // Initial configuration with telemetry plugin
    let initial_config = r#"
telemetry:
  exporters:
    metrics:
      prometheus:
        enabled: true
"#;

    // Updated configuration - change prometheus setting to trigger reload
    let updated_config = r#"
telemetry:
  exporters:
    metrics:
      prometheus:
        enabled: false
"#;

    let mut router = IntegrationTest::builder()
        .config(initial_config)
        .log("error,apollo_router=info,apollo_router::plugins::telemetry=debug")
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Verify initial startup log - telemetry plugin should log no previous config
    router.assert_log_contained("Telemetry plugin initial startup without previous configuration");

    // Update configuration to trigger hot reload
    router.update_config(updated_config).await;
    router.assert_reloaded().await;

    // Verify that telemetry plugin received previous configuration during reload
    router.assert_log_contained("Telemetry plugin reload detected with previous configuration");

    router.graceful_shutdown().await;
    Ok(())
}

/// When coprocessor has router_http configured, the router logs a one-time warning at startup.
/// Skipped when GraphOS is not enabled (coprocessor requires a GraphOS license).
#[tokio::test(flavor = "multi_thread")]
async fn test_router_http_customization_warning_logged() {
    if !graph_os_enabled() {
        return;
    }
    let config = r#"
coprocessor:
  url: http://127.0.0.1:9999
  router_http:
    request:
      headers: true
"#;

    let mut router = IntegrationTest::builder()
        .config(config.to_string())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    router.assert_log_contained("RouterHttp customizations are in use");

    router.graceful_shutdown().await;
}
