#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

extern crate core;

mod common;

use std::collections::HashSet;
use std::time::Duration;

use anyhow::anyhow;
use serde_json::json;
use serde_json::Value;
use tower::BoxError;

use crate::common::IntegrationTest;
use crate::common::Telemetry;
use crate::common::ValueExt;

#[tokio::test(flavor = "multi_thread")]
async fn test_reload() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/jaeger.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    for _ in 0..2 {
        let (id, result) = router.execute_query(&query).await;
        assert!(!result
            .headers()
            .get("apollo-custom-trace-id")
            .unwrap()
            .is_empty());
        validate_trace(
            id,
            &query,
            Some("ExampleQuery"),
            &["my_app", "router", "products"],
        )
        .await?;
        router.touch_config().await;
        router.assert_reloaded().await;
    }
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_remote_root() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/jaeger-no-sample.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    let (id, result) = router.execute_query(&query).await;
    assert!(!result
        .headers()
        .get("apollo-custom-trace-id")
        .unwrap()
        .is_empty());
    validate_trace(
        id,
        &query,
        Some("ExampleQuery"),
        &["my_app", "router", "products"],
    )
    .await?;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_local_root() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/jaeger.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    let (id, result) = router.execute_untraced_query(&query).await;
    assert!(!result
        .headers()
        .get("apollo-custom-trace-id")
        .unwrap()
        .is_empty());
    validate_trace(id, &query, Some("ExampleQuery"), &["router", "products"]).await?;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_local_root_no_sample() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/jaeger-no-sample.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    let (_, response) = router.execute_untraced_query(&query).await;
    assert!(response.headers().get("apollo-custom-trace-id").is_none());

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_local_root_50_percent_sample() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/jaeger-0.5-sample.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    let query = json!({"query":"query ExampleQuery {topProducts{name}}\n","variables":{}, "operationName": "ExampleQuery"});

    for _ in 0..100 {
        let (id, result) = router.execute_untraced_query(&query).await;

        if result.headers().get("apollo-custom-trace-id").is_some()
            && validate_trace(id, &query, Some("ExampleQuery"), &["router", "products"])
                .await
                .is_ok()
        {
            router.graceful_shutdown().await;

            return Ok(());
        }
    }
    panic!("tried 100 requests with telemetry sampled at 50%, no traces were found")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_no_telemetry() -> Result<(), BoxError> {
    // This test is currently skipped because it will only pass once we default the sampler to always off if there are no exporters.
    // Once this is fixed then we can re-enable
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/no-telemetry.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    let (_, response) = router.execute_untraced_query(&query).await;
    assert!(response.headers().get("apollo-custom-trace-id").is_none());

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_default_operation() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/jaeger.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    let query = json!({"query":"query ExampleQuery1 {topProducts{name}}","variables":{}});

    let (id, result) = router.execute_query(&query).await;
    assert!(!result
        .headers()
        .get("apollo-custom-trace-id")
        .unwrap()
        .is_empty());
    validate_trace(
        id,
        &query,
        Some("ExampleQuery1"),
        &["my_app", "router", "products"],
    )
    .await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_anonymous_operation() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/jaeger.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query {topProducts{name}}","variables":{}});

    let (id, result) = router.execute_query(&query).await;
    assert!(!result
        .headers()
        .get("apollo-custom-trace-id")
        .unwrap()
        .is_empty());
    validate_trace(id, &query, None, &["my_app", "router", "products"]).await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_selected_operation() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/jaeger.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    let query = json!({"query":"query ExampleQuery1 {topProducts{name}}\nquery ExampleQuery2 {topProducts{name}}","variables":{}, "operationName": "ExampleQuery2"});

    let (id, result) = router.execute_query(&query).await;
    assert!(!result
        .headers()
        .get("apollo-custom-trace-id")
        .unwrap()
        .is_empty());
    validate_trace(
        id,
        &query,
        Some("ExampleQuery2"),
        &["my_app", "router", "products"],
    )
    .await?;
    router.graceful_shutdown().await;
    Ok(())
}

async fn validate_trace(
    id: String,
    query: &Value,
    operation_name: Option<&str>,
    services: &[&'static str],
) -> Result<(), BoxError> {
    let params = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("service", services.first().expect("expected root service"))
        .finish();

    let url = format!("http://localhost:16686/api/traces/{id}?{params}");
    for _ in 0..10 {
        if find_valid_trace(&url, query, operation_name, services)
            .await
            .is_ok()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    find_valid_trace(&url, query, operation_name, services).await?;
    Ok(())
}

async fn find_valid_trace(
    url: &str,
    query: &Value,
    operation_name: Option<&str>,
    services: &[&'static str],
) -> Result<(), BoxError> {
    // A valid trace has:
    // * All three services
    // * The correct spans
    // * All spans are parented
    // * Required attributes of 'router' span has been set

    let trace: Value = reqwest::get(url)
        .await
        .map_err(|e| anyhow!("failed to contact jaeger; {}", e))?
        .json()
        .await?;
    tracing::debug!("{}", serde_json::to_string_pretty(&trace)?);

    // Verify that we got all the participants in the trace
    verify_trace_participants(&trace, services)?;

    // Verify that we got the expected span operation names
    verify_spans_present(&trace, operation_name, services)?;

    // Verify that all spans have a path to the root 'client_request' span
    verify_span_parenting(&trace, services)?;

    // Verify that root span fields are present
    verify_root_span_fields(&trace, operation_name)?;

    // Verify that supergraph span fields are present
    verify_supergraph_span_fields(&trace, query, operation_name)?;

    // Verify that router span fields are present
    verify_router_span_fields(&trace)?;

    Ok(())
}

fn verify_router_span_fields(trace: &Value) -> Result<(), BoxError> {
    let router_span = trace.select_path("$..spans[?(@.operationName == 'router')]")?[0];
    // We can't actually assert the values on a span. Only that a field has been set.
    assert_eq!(
        router_span
            .select_path("$.tags[?(@.key == 'client.name')].value")?
            .get(0),
        Some(&&Value::String("custom_name".to_string()))
    );
    assert_eq!(
        router_span
            .select_path("$.tags[?(@.key == 'client.version')].value")?
            .get(0),
        Some(&&Value::String("1.0".to_string()))
    );

    Ok(())
}

fn verify_root_span_fields(trace: &Value, operation_name: Option<&str>) -> Result<(), BoxError> {
    // We can't actually assert the values on a span. Only that a field has been set.
    let root_span_name = operation_name
        .map(|name| format!("query {}", name))
        .unwrap_or("query".to_string());
    let request_span = trace.select_path(&format!(
        "$..spans[?(@.operationName == '{root_span_name}')]"
    ))?[0];

    if let Some(operation_name) = operation_name {
        assert_eq!(
            request_span
                .select_path("$.tags[?(@.key == 'graphql.operation.name')].value")?
                .get(0),
            Some(&&Value::String(operation_name.to_string()))
        );
    } else {
        assert!(request_span
            .select_path("$.tags[?(@.key == 'graphql.operation.name')].value")?
            .get(0)
            .is_none(),);
    }

    assert_eq!(
        request_span
            .select_path("$.tags[?(@.key == 'graphql.operation.type')].value")?
            .get(0),
        Some(&&Value::String("query".to_string()))
    );

    Ok(())
}

fn verify_supergraph_span_fields(
    trace: &Value,
    query: &Value,
    operation_name: Option<&str>,
) -> Result<(), BoxError> {
    // We can't actually assert the values on a span. Only that a field has been set.
    let supergraph_span = trace.select_path("$..spans[?(@.operationName == 'supergraph')]")?[0];

    if let Some(operation_name) = operation_name {
        assert_eq!(
            supergraph_span
                .select_path("$.tags[?(@.key == 'graphql.operation.name')].value")?
                .get(0),
            Some(&&Value::String(operation_name.to_string()))
        );
    } else {
        assert!(supergraph_span
            .select_path("$.tags[?(@.key == 'graphql.operation.name')].value")?
            .get(0)
            .is_none(),);
    }

    assert_eq!(
        supergraph_span
            .select_path("$.tags[?(@.key == 'graphql.document')].value")?
            .get(0),
        Some(&&Value::String(
            query
                .as_object()
                .expect("should have been an object")
                .get("query")
                .expect("must have a query")
                .as_str()
                .expect("must be a string")
                .to_string()
        ))
    );

    Ok(())
}

fn verify_trace_participants(trace: &Value, services: &[&'static str]) -> Result<(), BoxError> {
    let actual_services: HashSet<String> = trace
        .select_path("$..serviceName")?
        .into_iter()
        .filter_map(|service| service.as_string())
        .collect();
    tracing::debug!("found services {:?}", actual_services);

    let expected_services = services
        .iter()
        .map(|s| s.to_string())
        .collect::<HashSet<_>>();
    if actual_services != expected_services {
        return Err(BoxError::from(format!(
            "incomplete traces, got {actual_services:?} expected {expected_services:?}"
        )));
    }
    Ok(())
}

fn verify_spans_present(
    trace: &Value,
    operation_name: Option<&str>,
    services: &[&'static str],
) -> Result<(), BoxError> {
    let operation_names: HashSet<String> = trace
        .select_path("$..operationName")?
        .into_iter()
        .filter_map(|span_name| span_name.as_string())
        .collect();
    let mut expected_operation_names: HashSet<String> = HashSet::from(
        [
            "execution",
            "HTTP POST",
            operation_name
                .map(|name| format!("query {name}"))
                .unwrap_or("query".to_string())
                .as_str(),
            "supergraph",
            "fetch",
            //"parse_query", Parse query will only happen once
            //"query_planning", query planning will only happen once
            "subgraph",
        ]
        .map(|s| s.into()),
    );
    if services.contains(&"my_app") {
        expected_operation_names.insert("client_request".into());
    }
    tracing::debug!("found spans {:?}", operation_names);
    let missing_operation_names: Vec<_> = expected_operation_names
        .iter()
        .filter(|o| !operation_names.contains(*o))
        .collect();
    if !missing_operation_names.is_empty() {
        return Err(BoxError::from(format!(
            "spans did not match, got {operation_names:?}, missing {missing_operation_names:?}"
        )));
    }
    Ok(())
}

fn verify_span_parenting(trace: &Value, services: &[&'static str]) -> Result<(), BoxError> {
    let root_span = if services.contains(&"my_app") {
        trace.select_path("$..spans[?(@.operationName == 'client_request')]")?[0]
    } else {
        trace.select_path("$..spans[?(@.operationName == 'query ExampleQuery')]")?[0]
    };
    let spans = trace.select_path("$..spans[*]")?;
    for span in spans {
        let mut span_path = vec![span.select_path("$.operationName")?[0]
            .as_str()
            .expect("operation name not not found")];
        let mut current = span;
        while let Some(parent) = parent_span(trace, current) {
            span_path.push(
                parent.select_path("$.operationName")?[0]
                    .as_str()
                    .expect("operation name not not found"),
            );
            current = parent;
        }
        tracing::debug!("span path to root: '{:?}'", span_path);
        if current != root_span {
            return Err(BoxError::from(format!(
                "span {:?} did not have a path to the root span",
                span.select_path("$.operationName")?,
            )));
        }
    }
    Ok(())
}

fn parent_span<'a>(trace: &'a Value, span: &'a Value) -> Option<&'a Value> {
    span.select_path("$.references[?(@.refType == 'CHILD_OF')].spanID")
        .ok()?
        .into_iter()
        .filter_map(|id| id.as_str())
        .filter_map(|id| {
            trace
                .select_path(&format!("$..spans[?(@.spanID == '{id}')]"))
                .ok()?
                .into_iter()
                .next()
        })
        .next()
}
