use std::process::Command;
use std::time::Duration;

use rdkafka::ClientConfig;
use rdkafka::producer::FutureProducer;
use rdkafka::producer::FutureRecord;
use rdkafka::util::Timeout;
use serde_json::json;
use tower::BoxError;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;

use crate::integration::common::IntegrationTest;
use crate::integration::common::Query;

const EVENT_PAYLOAD: &str = r#"{"__typename":"Product","id":"1"}"#;

fn event_config(
    provider_type: &str,
    provider_config: serde_json::Value,
    provider_options: serde_json::Value,
) -> String {
    json!({
        "supergraph": {"listen": "127.0.0.1:4000", "path": "/"},
        "homepage": {"enabled": false},
        "sandbox": {"enabled": false},
        "subscription": {"enabled": true},
        "events": {
            "providers": {
                "broker": {"type": provider_type, "config": provider_config}
            },
            "sources": {
                "product-updates": {
                    "provider": "broker",
                    "policy": "live",
                    "provider_options": provider_options
                }
            },
            "policies": {"live": {}}
        }
    })
    .to_string()
}

struct BrokerContainer(String);

struct HydrationServices {
    products: MockServer,
    reviews: MockServer,
}

impl HydrationServices {
    async fn start() -> Self {
        let products = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"_entities": [{"name": "Table"}]}
            })))
            .mount(&products)
            .await;

        let reviews = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"_entities": [{"reviews": [{"body": "Excellent"}]}]}
            })))
            .mount(&reviews)
            .await;

        Self { products, reviews }
    }

    fn overrides(&self) -> std::collections::HashMap<String, String> {
        [
            ("products".to_string(), self.products.uri()),
            ("reviews".to_string(), self.reviews.uri()),
        ]
        .into()
    }

    async fn assert_both_called(&self) {
        assert!(
            !self.products.received_requests().await.unwrap().is_empty(),
            "products subgraph was not called"
        );
        assert!(
            !self.reviews.received_requests().await.unwrap().is_empty(),
            "reviews subgraph was not called"
        );
    }
}

impl BrokerContainer {
    fn start(image: &str, options: &[String], command: &[&str]) -> Self {
        let output = Command::new("docker")
            .args(["run", "--detach", "--rm"])
            .args(options)
            .arg(image)
            .args(command)
            .output()
            .expect("Docker is required for these ignored integration tests");
        assert!(
            output.status.success(),
            "failed to start {image}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Self(String::from_utf8(output.stdout).unwrap().trim().to_string())
    }

    fn exec(&self, args: &[&str]) -> std::process::Output {
        Command::new("docker")
            .arg("exec")
            .arg(&self.0)
            .args(args)
            .output()
            .expect("docker exec starts")
    }
}

impl Drop for BrokerContainer {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "--force", &self.0])
            .status();
    }
}

async fn build_router(config: String) -> (IntegrationTest, HydrationServices) {
    let services = HydrationServices::start().await;
    let router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/event_stream.graphql")
        .config(config)
        .subgraph_overrides(services.overrides())
        .build()
        .await;
    (router, services)
}

async fn subscribe(router: &IntegrationTest) -> reqwest::Response {
    let query = Query::builder()
        .body(json!({
            "query": "subscription ProductUpdated($id: ID!) { productUpdated(id: $id) { id name reviews { body } } }",
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
    response
}

async fn assert_hydrated_event(response: reqwest::Response) {
    let mut multipart = multer::Multipart::new(response.bytes_stream(), "graphql");
    let event = tokio::time::timeout(Duration::from_secs(20), async {
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
        Some(&json!({
            "id": "1",
            "name": "Table",
            "reviews": [{"body": "Excellent"}]
        }))
    );
}

async fn connect_nats(port: u16) -> async_nats::Client {
    let url = format!("nats://127.0.0.1:{port}");
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(client) = async_nats::connect(&url).await {
                break client;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("NATS did not become ready")
}

async fn wait_for_exec(container: &BrokerContainer, args: &[&str], service: &str) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if container.exec(args).status.success() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{service} did not become ready"));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker to run a real NATS broker"]
async fn nats_core_event_is_federated_and_hydrated() -> Result<(), BoxError> {
    let (mut router, services) = build_router(event_config(
        "nats_core",
        json!({"servers": ["nats://127.0.0.1:{{BROKER_PORT}}"]}),
        json!({}),
    ))
    .await;
    let port = router.reserve_address("BROKER_PORT");
    let _broker = BrokerContainer::start(
        "nats:2.11",
        &["--publish".into(), format!("127.0.0.1:{port}:4222")],
        &[],
    );
    let client = connect_nats(port).await;

    router.start().await;
    router.assert_started().await;
    let response = subscribe(&router).await;
    client.publish("products.1", EVENT_PAYLOAD.into()).await?;
    client.flush().await?;
    assert_hydrated_event(response).await;
    services.assert_both_called().await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker to run a real NATS JetStream broker"]
async fn nats_jetstream_event_is_federated_and_hydrated() -> Result<(), BoxError> {
    let (mut router, services) = build_router(event_config(
        "nats_jetstream",
        json!({"servers": ["nats://127.0.0.1:{{BROKER_PORT}}"]}),
        json!({"stream": "PRODUCTS"}),
    ))
    .await;
    let port = router.reserve_address("BROKER_PORT");
    let _broker = BrokerContainer::start(
        "nats:2.11",
        &["--publish".into(), format!("127.0.0.1:{port}:4222")],
        &["--jetstream"],
    );
    let client = connect_nats(port).await;
    let jetstream = async_nats::jetstream::new(client);
    jetstream
        .create_stream(async_nats::jetstream::stream::Config {
            name: "PRODUCTS".to_string(),
            subjects: vec!["products.*".to_string()],
            ..Default::default()
        })
        .await?;

    router.start().await;
    router.assert_started().await;
    let response = subscribe(&router).await;
    jetstream
        .publish("products.1", EVENT_PAYLOAD.into())
        .await?
        .await?;
    assert_hydrated_event(response).await;
    services.assert_both_called().await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker to run a real Redis broker"]
async fn redis_pubsub_event_is_federated_and_hydrated() -> Result<(), BoxError> {
    let (mut router, services) = build_router(event_config(
        "redis_pubsub",
        json!({"url": "redis://127.0.0.1:{{BROKER_PORT}}"}),
        json!({}),
    ))
    .await;
    let port = router.reserve_address("BROKER_PORT");
    let broker = BrokerContainer::start(
        "redis:7.4-alpine",
        &["--publish".into(), format!("127.0.0.1:{port}:6379")],
        &[],
    );
    wait_for_exec(&broker, &["redis-cli", "PING"], "Redis").await;

    router.start().await;
    router.assert_started().await;
    let response = subscribe(&router).await;
    let published = broker.exec(&["redis-cli", "PUBLISH", "products.1", EVENT_PAYLOAD]);
    assert!(published.status.success(), "Redis publish failed");
    assert_hydrated_event(response).await;
    services.assert_both_called().await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker to run a real Kafka broker"]
async fn kafka_event_is_federated_and_hydrated() -> Result<(), BoxError> {
    let (mut router, services) = build_router(event_config(
        "kafka",
        json!({"bootstrap_servers": ["127.0.0.1:{{BROKER_PORT}}"]}),
        json!({}),
    ))
    .await;
    let port = router.reserve_address("BROKER_PORT");
    let broker = BrokerContainer::start(
        "apache/kafka:3.9.1",
        &[
            "--publish".into(),
            format!("127.0.0.1:{port}:9092"),
            "--env".into(),
            "KAFKA_NODE_ID=1".into(),
            "--env".into(),
            "KAFKA_PROCESS_ROLES=broker,controller".into(),
            "--env".into(),
            "KAFKA_LISTENERS=INTERNAL://:29092,EXTERNAL://:9092,CONTROLLER://:9093".into(),
            "--env".into(),
            format!(
                "KAFKA_ADVERTISED_LISTENERS=INTERNAL://localhost:29092,EXTERNAL://127.0.0.1:{port}"
            ),
            "--env".into(),
            "KAFKA_CONTROLLER_LISTENER_NAMES=CONTROLLER".into(),
            "--env".into(),
            "KAFKA_LISTENER_SECURITY_PROTOCOL_MAP=CONTROLLER:PLAINTEXT,INTERNAL:PLAINTEXT,EXTERNAL:PLAINTEXT".into(),
            "--env".into(),
            "KAFKA_INTER_BROKER_LISTENER_NAME=INTERNAL".into(),
            "--env".into(),
            "KAFKA_CONTROLLER_QUORUM_VOTERS=1@localhost:9093".into(),
            "--env".into(),
            "KAFKA_OFFSETS_TOPIC_REPLICATION_FACTOR=1".into(),
            "--env".into(),
            "KAFKA_TRANSACTION_STATE_LOG_REPLICATION_FACTOR=1".into(),
            "--env".into(),
            "KAFKA_TRANSACTION_STATE_LOG_MIN_ISR=1".into(),
            "--env".into(),
            "KAFKA_GROUP_INITIAL_REBALANCE_DELAY_MS=0".into(),
        ],
        &[],
    );
    wait_for_exec(
        &broker,
        &[
            "/opt/kafka/bin/kafka-topics.sh",
            "--bootstrap-server",
            "127.0.0.1:29092",
            "--list",
        ],
        "Kafka",
    )
    .await;
    let created = broker.exec(&[
        "/opt/kafka/bin/kafka-topics.sh",
        "--bootstrap-server",
        "127.0.0.1:29092",
        "--create",
        "--if-not-exists",
        "--topic",
        "products.1",
        "--partitions",
        "1",
        "--replication-factor",
        "1",
    ]);
    assert!(created.status.success(), "Kafka topic creation failed");

    router.start().await;
    router.assert_started().await;
    let response = subscribe(&router).await;
    let producer: FutureProducer = ClientConfig::new()
        .set("bootstrap.servers", format!("127.0.0.1:{port}"))
        .set("message.timeout.ms", "5000")
        .create()?;
    for sequence in 0..20 {
        producer
            .send(
                FutureRecord::to("products.1")
                    .payload(EVENT_PAYLOAD)
                    .key(&format!("event-{sequence}")),
                Timeout::After(Duration::from_secs(5)),
            )
            .await
            .map_err(|(error, _)| error)?;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert_hydrated_event(response).await;
    services.assert_both_called().await;
    router.graceful_shutdown().await;
    Ok(())
}
