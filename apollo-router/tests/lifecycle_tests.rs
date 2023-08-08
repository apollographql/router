use std::path::Path;
use std::time::Duration;

use apollo_router::graphql;
use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInit;
use apollo_router::register_plugin;
use apollo_router::services::router;
use apollo_router::services::supergraph;
use apollo_router::Context;
use apollo_router::TestHarness;
use async_trait::async_trait;
use futures::FutureExt;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt;
use wiremock::ResponseTemplate;

use crate::common::IntegrationTest;

mod common;

const HAPPY_CONFIG: &str = include_str!("fixtures/jaeger.router.yaml");
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
    for i in 0..3 {
        println!("iteration {i}");
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
    }
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
async fn test_force_reload() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            "experimental_chaos:
                force_reload: 1s",
        )
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    router.assert_no_reload_necessary().await;
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
                experimental_logging:
                    format: json
            ",
        )
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router
        .assert_log_contains("You're using some \\\"experimental\\\" features of the Apollo Router")
        .await;
    router.graceful_shutdown().await;
}

const TEST_PLUGIN_ORDERING_CONTEXT_KEY: &str = "ordering-trace";

/// <https://github.com/apollographql/router/issues/3207>
#[tokio::test(flavor = "multi_thread")]
async fn test_plugin_ordering() {
    async fn coprocessor(
        request: hyper::Request<hyper::Body>,
    ) -> Result<hyper::Response<hyper::Body>, BoxError> {
        let body = hyper::body::to_bytes(request.into_body()).await?;
        let mut json: serde_json::Value = serde_json::from_slice(&body)?;
        let stage = json["stage"].as_str().unwrap().to_owned();
        json["context"]["entries"]
            .as_object_mut()
            .unwrap()
            .entry(TEST_PLUGIN_ORDERING_CONTEXT_KEY)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap()
            .push(format!("coprocessor {stage}").into());
        Ok(hyper::Response::new(hyper::Body::from(
            serde_json::to_string(&json)?,
        )))
    }

    async fn spawn_coprocessor() -> (String, ShutdownOnDrop) {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let shutdown_on_drop = ShutdownOnDrop(Some(tx));
        let service = hyper::service::make_service_fn(|_| async {
            Ok::<_, hyper::Error>(hyper::service::service_fn(coprocessor))
        });
        // Bind to "port 0" to let the kernel choose an available port number.
        let server = hyper::Server::bind(&([127, 0, 0, 1], 0).into()).serve(service);
        let coprocessor_url = format!("http://{}", server.local_addr());
        let server = server.with_graceful_shutdown(async {
            let _ = rx.await;
        });
        tokio::spawn(async move {
            if let Err(e) = server.await {
                eprintln!("coprocessor server error: {}", e);
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
                "router": {
                    "request": { "context": true },
                    "response": { "context": true },
                }
            },
        }))
        .unwrap()
        .build_router()
        .await
        .unwrap();
    // Repeat to get more confidence it’s deterministic
    for _ in 0..10 {
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
        assert_eq!(
            trace,
            [
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
            ]
        );
    }
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
