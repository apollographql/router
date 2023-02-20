extern crate core;

mod common;

use std::collections::HashSet;
use std::time::Duration;

use anyhow::anyhow;
use serde_json::json;
use serde_json::Value;
use tower::BoxError;

use crate::common::IntegrationTest;
use crate::common::ValueExt;

#[tokio::test(flavor = "multi_thread")]
async fn test_jaeger_tracing() -> Result<(), BoxError> {
    let tracer = opentelemetry_jaeger::new_agent_pipeline()
        .with_service_name("my_app")
        .install_simple()?;

    let mut router = IntegrationTest::new(
        tracer,
        opentelemetry_jaeger::Propagator::new(),
        include_str!("fixtures/jaeger.router.yaml"),
    )
    .await;

    router.start().await;
    router.assert_started().await;

    for _ in 0..2 {
        let (id, result) = router.run_query().await;
        assert!(!result
            .headers()
            .get("apollo-custom-trace-id")
            .unwrap()
            .is_empty());
        query_jaeger_for_trace(id).await?;
        router.touch_config().await;
        router.assert_reloaded().await;
    }
    router.graceful_shutdown().await;
    Ok(())
}

async fn query_jaeger_for_trace(id: String) -> Result<(), BoxError> {
    let tags = json!({ "unit_test": id });
    let params = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("service", "my_app")
        .append_pair("tags", &tags.to_string())
        .finish();

    let url = format!("http://localhost:16686/api/traces?{params}");
    for _ in 0..10 {
        if find_valid_trace(&url).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    find_valid_trace(&url).await?;
    Ok(())
}

async fn find_valid_trace(url: &str) -> Result<(), BoxError> {
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
    verify_spans_present(&trace)?;

    // Verify that all spans have a path to the root 'client_request' span
    verify_span_parenting(&trace)?;

    // Verify that supergraph span fields are present
    verify_supergraph_span_fields(&trace)?;

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

fn verify_supergraph_span_fields(trace: &Value) -> Result<(), BoxError> {
    let supergraph_span = trace.select_path("$..spans[?(@.operationName == 'supergraph')]")?[0];
    // We can't actually assert the values on a span. Only that a field has been set.
    assert_eq!(
        supergraph_span
            .select_path("$.tags[?(@.key == 'graphql.document')].value")?
            .get(0),
        Some(&&Value::String("{topProducts{name}}".to_string()))
    );
    assert_eq!(
        supergraph_span
            .select_path("$.tags[?(@.key == 'graphql.operation.name')].value")?
            .get(0),
        Some(&&Value::String("".to_string()))
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

fn verify_spans_present(trace: &Value) -> Result<(), BoxError> {
    let operation_names: HashSet<String> = trace
        .select_path("$..operationName")?
        .into_iter()
        .filter_map(|span_name| span_name.as_string())
        .collect();
    let expected_operation_names: HashSet<String> = HashSet::from(
        [
            "execution",
            "HTTP POST",
            "request",
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
