mod common;

use std::collections::HashMap;
use std::collections::HashSet;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;
use std::time::SystemTime;

use http::Request;
use http::Response;
use http::StatusCode;
use hyper::server::Server;
use hyper::service::make_service_fn;
use hyper::service::service_fn;
use hyper::Body;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::Span;
use opentelemetry::trace::Tracer;
use opentelemetry::trace::TracerProvider;
use serde_json::json;
use serde_json::Value;
use tower::BoxError;

use crate::common::TracingTest;
use crate::common::ValueExt;

#[tokio::test(flavor = "multi_thread")]
async fn test_jaeger_tracing() -> Result<(), BoxError> {
    let tracer = opentelemetry_jaeger::new_pipeline()
        .with_service_name("my_app")
        .install_simple()?;

    let router = TracingTest::new(
        tracer,
        opentelemetry_jaeger::Propagator::new(),
        Path::new("jaeger.router.yaml"),
    );

    tokio::task::spawn(subgraph());

    for _ in 0..10 {
        let id = router.run_query().await;
        query_jaeger_for_trace(id).await?;
        router.touch_config()?;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

async fn query_jaeger_for_trace(id: String) -> Result<(), BoxError> {
    let tags = json!({ "unit_test": id });
    let params = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("service", "my_app")
        .append_pair("tags", &tags.to_string())
        .finish();

    let url = format!("http://localhost:16686/api/traces?{}", params);
    for _ in 0..10 {
        match find_valid_trace(&url).await {
            Ok(_) => {
                return Ok(());
            }
            Err(e) => {
                println!("error: {}", e);
                tracing::warn!("{}", e);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("did not get full otel trace");
}

async fn find_valid_trace(url: &str) -> Result<(), BoxError> {
    // A valid trace has:
    // * All three services
    // * The correct spans
    // * All spans are parented
    // * Required attributes of 'router' span has been set

    let trace: Value = reqwest::get(url).await?.json().await?;
    tracing::debug!("{}", serde_json::to_string_pretty(&trace)?);

    // Verify that we got all the participants in the trace
    verify_trace_participants(&trace)?;

    // Verify that we got the expected span operation names
    verify_spans_present(&trace)?;

    // Verify that all spans have a path to the root 'client_request' span
    verify_span_parenting(&trace)?;

    // Verify that router span fields are present
    verify_router_span_fields(&trace)?;

    Ok(())
}

fn verify_router_span_fields(trace: &Value) -> Result<(), BoxError> {
    let router_span = trace.select_path("$..spans[?(@.operationName == 'router')]")?[0];
    // We can't actually assert the values on a span. Only that a field has been set.
    assert_eq!(
        router_span
            .select_path("$.tags[?(@.key == 'query')].value")?
            .get(0),
        Some(&&Value::String("{topProducts{name}}".to_string()))
    );
    assert_eq!(
        router_span
            .select_path("$.tags[?(@.key == 'operation_name')].value")?
            .get(0),
        Some(&&Value::String("".to_string()))
    );
    assert_eq!(
        router_span
            .select_path("$.tags[?(@.key == 'client_name')].value")?
            .get(0),
        Some(&&Value::String("custom_name".to_string()))
    );
    assert_eq!(
        router_span
            .select_path("$.tags[?(@.key == 'client_version')].value")?
            .get(0),
        Some(&&Value::String("1.0".to_string()))
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
            "incomplete traces, got {:?} expected {:?}",
            services, expected_services
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
            "router",
            "fetch",
            //"parse_query", Parse query will only happen once
            "query_planning",
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
            "spans did not match, got {:?}, missing {:?}",
            operation_names, missing_operation_names
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
                .select_path(&format!("$..spans[?(@.spanID == '{}')]", id))
                .ok()?
                .into_iter()
                .next()
        })
        .next()
}

// starts a local server emulating the products subgraph
async fn subgraph() {
    async fn handle(request: Request<Body>) -> Result<Response<Body>, Infallible> {
        // create the opentelemetry-jaeger tracing infrastructure
        let tracer_provider = opentelemetry_jaeger::new_pipeline()
            .with_service_name("products")
            .build_simple()
            .unwrap();
        let tracer = tracer_provider.tracer("products");

        //extract the trace id from headers and create a child span from it
        println!("headers: {:?}", request.headers());
        assert!(
            request.headers().get("uber-trace-id").is_some(),
            "the uber-trace-id is absent, trace propagation is broken"
        );

        let headers: HashMap<String, String> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap().to_string(),
                )
            })
            .collect();
        let context = opentelemetry_jaeger::Propagator::new().extract(&headers);
        let mut span = tracer.start_with_context("HTTP POST", &context);
        tokio::time::sleep(Duration::from_millis(2)).await;
        span.end_with_timestamp(SystemTime::now());
        println!("flush result: {:?}", tracer_provider.force_flush());

        // send the response
        let body_bytes = hyper::body::to_bytes(request.into_body()).await.unwrap();
        assert_eq!(
            r#"{"query":"{topProducts{name}}"}"#,
            std::str::from_utf8(&body_bytes).unwrap()
        );
        Ok(Response::builder()
            .header("Content-Type", "application/json")
            .status(StatusCode::OK)
            .body(
                r#"{"data":{"topProducts":[{"name":"Table"},{"name":"Couch"},{"name":"Chair"}]}}"#
                    .into(),
            )
            .unwrap())
    }

    let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
    let server = Server::bind(&SocketAddr::from(([127, 0, 0, 1], 4005))).serve(make_svc);
    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }
}
