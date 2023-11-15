//! This file is to load test subscriptions and should be launched manually, not in our CI
use futures::StreamExt;
use http::HeaderValue;
use serde_json::json;
use tower::BoxError;

use crate::common::IntegrationTest;
use crate::common::Telemetry;

mod common;

const SUBSCRIPTION_CONFIG: &str = include_str!("fixtures/subscription.router.yaml");
const SUB_QUERY: &str =
    r#"subscription {  userWasCreated(intervalMs: 5, nbEvents: 10) {    name reviews { body } }}"#;
const UNFEDERATED_SUB_QUERY: &str = r#"subscription {  userWasCreated { name username }}"#;

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription() -> Result<(), BoxError> {
    if std::env::var("TEST_APOLLO_KEY").is_ok() && std::env::var("TEST_APOLLO_GRAPH_REF").is_ok() {
        let mut router = create_router(SUBSCRIPTION_CONFIG).await?;
        router.start().await;
        router.assert_started().await;

        let (_, response) = router.run_subscription(SUB_QUERY).await;
        assert!(response.status().is_success());

        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            assert!(chunk.starts_with(b"\r\n--graphql\r\ncontent-type: application/json\r\n\r\n"));
            assert!(chunk.ends_with(b"\r\n--graphql--\r\n"));
        }
    }

    Ok(())
}

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_load() -> Result<(), BoxError> {
    let mut router = create_router(SUBSCRIPTION_CONFIG).await?;
    router.start().await;
    router.assert_started().await;

    for i in 0..1000000i64 {
        let (_, response) = router.run_subscription(UNFEDERATED_SUB_QUERY).await;
        assert!(response.status().is_success());
        assert_eq!(
            response.headers().get("x-accel-buffering").unwrap(),
            &HeaderValue::from_static("no")
        );

        tokio::spawn(async move {
            let mut stream = response.bytes_stream();
            while let Some(_chunk) = stream.next().await {}
        });
        if i % 100 == 0 {
            println!("iii - {i}");
        }
    }

    for _ in 0..100 {
        let (_id, resp) = router
            .execute_query(
                &json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}}),
            )
            .await;
        assert!(resp.status().is_success());
    }

    Ok(())
}

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_load_federated() -> Result<(), BoxError> {
    let mut router = create_router(SUBSCRIPTION_CONFIG).await?;
    router.start().await;
    router.assert_started().await;

    for i in 0..1000000i64 {
        let (_, response) = router.run_subscription(SUB_QUERY).await;
        assert!(response.status().is_success());

        tokio::spawn(async move {
            let mut stream = response.bytes_stream();
            while let Some(_chunk) = stream.next().await {}
        });
        if i % 100 == 0 {
            println!("iii - {i}");
        }
    }

    for _ in 0..100 {
        let (_id, resp) = router.execute_default_query().await;
        assert!(resp.status().is_success());
    }

    Ok(())
}

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_with_dedup_load_standalone() -> Result<(), BoxError> {
    for i in 0..1000000i64 {
        let response = run_subscription(UNFEDERATED_SUB_QUERY, None).await;
        assert!(
            response.status().is_success(),
            "error status {:?}",
            response.status()
        );

        tokio::spawn(async move {
            let mut stream = response.bytes_stream();
            while let Some(_chunk) = stream.next().await {}
        });
        if i % 100 == 0 {
            println!("iii - {i}");
        }
    }

    Ok(())
}

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_memory_usage() -> Result<(), BoxError> {
    for i in 0..300i64 {
        let response = run_subscription(SUB_QUERY, None).await;
        assert!(
            response.status().is_success(),
            "error status {:?}",
            response.status()
        );

        if i == 299 {
            let mut stream = response.bytes_stream();
            while let Some(_chunk) = stream.next().await {}
        } else {
            tokio::spawn(async move {
                let mut stream = response.bytes_stream();
                while let Some(_chunk) = stream.next().await {}
            });
        }
        if i % 100 == 0 {
            println!("iii - {i}");
        }
    }

    Ok(())
}

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_without_dedup_load_standalone() -> Result<(), BoxError> {
    for i in 0..1000000i64 {
        let response = run_subscription(UNFEDERATED_SUB_QUERY, Some(i)).await;
        assert!(
            response.status().is_success(),
            "error status {:?}",
            response.status()
        );

        tokio::spawn(async move {
            let mut stream = response.bytes_stream();
            while let Some(_chunk) = stream.next().await {}
        });
        if i % 100 == 0 {
            println!("iii - {i}");
        }
    }

    Ok(())
}

async fn create_router(config: &'static str) -> Result<IntegrationTest, BoxError> {
    Ok(IntegrationTest::builder()
        .telemetry(Telemetry::Jaeger)
        .config(config)
        .build()
        .await)
}

async fn run_subscription(sub_query: &str, id: Option<i64>) -> reqwest::Response {
    let client = reqwest::Client::new();

    let mut request = client
        .post("http://localhost:4000")
        .header(
            "accept",
            "multipart/mixed;boundary=\"graphql\";subscriptionSpec=1.0",
        )
        .header("apollographql-client-name", "custom_name")
        .header("apollographql-client-version", "1.0")
        .json(&json!({"query":sub_query,"variables":{}}));

    // Introduce a header to generate a different sub and then disable dedup
    if let Some(id) = id {
        request = request.header("custom_id", format!("{id}"));
    }

    let request = request.build().unwrap();

    match client.execute(request).await {
        Ok(response) => response,
        Err(err) => {
            panic!("unable to send successful request to router, {err}")
        }
    }
}
