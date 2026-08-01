use std::process::Command;
use std::time::Duration;

use serde_json::json;
use tower::BoxError;
use wiremock::ResponseTemplate;

use crate::integration::common::IntegrationTest;
use crate::integration::common::Query;

const CONFIG: &str = r#"
supergraph:
  listen: 127.0.0.1:4000
  path: /
homepage:
  enabled: false
sandbox:
  enabled: false
subscription:
  enabled: true
events:
  providers:
    broker:
      type: nats_core
      config:
        servers: ["nats://127.0.0.1:{{NATS_PORT}}"]
  sources:
    product-updates:
      provider: broker
      policy: live
  policies:
    live: {}
"#;

struct NatsContainer(String);

impl NatsContainer {
    fn start(port: u16) -> Self {
        let output = Command::new("docker")
            .args([
                "run",
                "--detach",
                "--rm",
                "--publish",
                &format!("127.0.0.1:{port}:4222"),
                "nats:2.11",
            ])
            .output()
            .expect("Docker is required for this ignored integration test");
        assert!(
            output.status.success(),
            "failed to start NATS: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Self(String::from_utf8(output.stdout).unwrap().trim().to_string())
    }
}

impl Drop for NatsContainer {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "--force", &self.0])
            .status();
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker to run a real NATS broker"]
async fn nats_core_event_is_federated_and_hydrated() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/event_stream.graphql")
        .config(CONFIG)
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"_entities": [{"name": "Table"}]}
        })))
        .build()
        .await;
    let nats_port = router.reserve_address("NATS_PORT");
    let _nats = NatsContainer::start(nats_port);
    let nats_url = format!("nats://127.0.0.1:{nats_port}");
    let client = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(client) = async_nats::connect(&nats_url).await {
                break client;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("NATS did not become ready");

    router.start().await;
    router.assert_started().await;

    let query = Query::builder()
        .body(json!({
            "query": "subscription ProductUpdated($id: ID!) { productUpdated(id: $id) { id name } }",
            "variables": {"id": "1"}
        }))
        .headers([(
            "Accept".to_string(),
            "multipart/mixed;subscriptionSpec=1.0".to_string(),
        )]
        .into())
        .build();
    let (_, response) = router.execute_query(query).await;
    assert!(response.status().is_success());

    client
        .publish("products.1", r#"{"__typename":"Product","id":"1"}"#.into())
        .await?;
    client.flush().await?;

    let mut multipart = multer::Multipart::new(response.bytes_stream(), "graphql");
    let event = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let field = multipart
                .next_field()
                .await
                .expect("valid multipart response")
                .expect("subscription response ended before an event");
            let value: serde_json::Value = field.json().await.expect("valid JSON event");
            if value != json!({}) {
                break value;
            }
        }
    })
    .await
    .expect("timed out waiting for the federated event");
    assert_eq!(
        event.pointer("/payload/data/productUpdated"),
        Some(&json!({"id": "1", "name": "Table"}))
    );

    drop(multipart);
    router.graceful_shutdown().await;
    Ok(())
}
