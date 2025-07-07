//! This file is to load-test subscriptions and should be launched manually, not in our CI
use futures::StreamExt;
use http::HeaderValue;
use serde_json::json;
use tower::BoxError;

use super::common::IntegrationTest;
use super::common::Query;
use super::common::Telemetry;

const SUBSCRIPTION_CONFIG: &str = include_str!("../fixtures/subscription.router.yaml");
const SUB_QUERY: &str =
    r#"subscription {  userWasCreated(intervalMs: 5, nbEvents: 10) {    name reviews { body } }}"#;
const UNFEDERATED_SUB_QUERY: &str = r#"subscription {  userWasCreated { name username }}"#;

fn is_json_field(field: &multer::Field<'_>) -> bool {
    field
        .content_type()
        .is_some_and(|mime| mime.essence_str() == "application/json")
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription() -> Result<(), BoxError> {
    if std::env::var("TEST_APOLLO_KEY").is_ok() && std::env::var("TEST_APOLLO_GRAPH_REF").is_ok() {
        let mut router = create_router(SUBSCRIPTION_CONFIG).await?;
        router.start().await;
        router.assert_started().await;

        let (_, response) = router.run_subscription(SUB_QUERY).await;
        assert!(response.status().is_success());

        // Use `multipart/form-data` parsing. The router actually responds with `multipart/mixed`, but
        // the formats are compatible.
        let mut multipart = multer::Multipart::new(response.bytes_stream(), "graphql");
        while let Some(field) = multipart
            .next_field()
            .await
            .expect("could not read next chunk")
        {
            assert!(is_json_field(&field), "all response chunks must be JSON");
            let _: serde_json::Value = field.json().await.expect("invalid JSON chunk");
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
                Query::builder()
                    .body(json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}}))
                    .build(),
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
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(config)
        .build()
        .await)
}

async fn run_subscription(sub_query: &str, id: Option<i64>) -> reqwest::Response {
    let client = reqwest::Client::new();

    let mut request = client
        .post("http://localhost:4000")
        .header("accept", "multipart/mixed;subscriptionSpec=1.0")
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
