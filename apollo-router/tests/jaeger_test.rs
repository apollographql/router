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
        validate_trace(id, &query, Some("ExampleQuery")).await?;
        router.touch_config().await;
        router.assert_reloaded().await;
    }
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
    validate_trace(id, &query, Some("ExampleQuery1")).await?;
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
    validate_trace(id, &query, None).await?;
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
    validate_trace(id, &query, Some("ExampleQuery2")).await?;
    router.graceful_shutdown().await;
    Ok(())
}

async fn validate_trace(
    id: String,
    query: &Value,
    operation_name: Option<&str>,
) -> Result<(), BoxError> {
    let tags = json!({ "unit_test": id });
    let params = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("service", "my_app")
        .append_pair("tags", &tags.to_string())
        .finish();

    let url = format!("http://localhost:16686/api/traces?{params}");
    for _ in 0..10 {
        if find_valid_trace(&url, query, operation_name).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    find_valid_trace(&url, query, operation_name).await?;
    Ok(())
}

async fn find_valid_trace(
    url: &str,
    query: &Value,
    operation_name: Option<&str>,
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
    verify_trace_participants(&trace)?;

    // Verify that we got the expected span operation names
    verify_spans_present(&trace, operation_name)?;

    // Verify that all spans have a path to the root 'client_request' span
    verify_span_parenting(&trace)?;

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

fn verify_trace_participants(trace: &Value) -> Result<(), BoxError> {
    let services: HashSet<String> = trace
        .select_path("$..serviceName")?
        .into_iter()
        .filter_map(|service| service.as_string())
        .collect();
    tracing::debug!("found services {:?}", services);

    let expected_services = HashSet::from(["my_app", "router", "products"].map(|s| s.into()));
    if services != expected_services {
        return Err(BoxError::from(format!(
            "incomplete traces, got {services:?} expected {expected_services:?}"
        )));
    }
    Ok(())
}

fn verify_spans_present(trace: &Value, operation_name: Option<&str>) -> Result<(), BoxError> {
    let operation_names: HashSet<String> = trace
        .select_path("$..operationName")?
        .into_iter()
        .filter_map(|span_name| span_name.as_string())
        .collect();
    let expected_operation_names: HashSet<String> = HashSet::from(
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
            "client_request",
        ]
        .map(|s| s.into()),
    );
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

fn verify_span_parenting(trace: &Value) -> Result<(), BoxError> {
    let root_span = trace.select_path("$..spans[?(@.operationName == 'client_request')]")?[0];
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
