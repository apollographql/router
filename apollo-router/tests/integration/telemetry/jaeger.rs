extern crate core;

use std::collections::HashSet;
use std::ops::Deref;

use anyhow::anyhow;
use opentelemetry_api::trace::TraceId;
use serde_json::Value;
use serde_json::json;
use tower::BoxError;

use crate::integration::IntegrationTest;
use crate::integration::ValueExt;
use crate::integration::common::Query;
use crate::integration::common::Telemetry;
use crate::integration::telemetry::TraceSpec;
use crate::integration::telemetry::verifier::Verifier;

#[tokio::test(flavor = "multi_thread")]
async fn test_reload() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/jaeger.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    for _ in 0..2 {
        TraceSpec::builder()
            .services(["client", "router", "subgraph"].into())
            .operation_name("ExampleQuery")
            .build()
            .validate_jaeger_trace(&mut router, Query::default())
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

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .build()
        .validate_jaeger_trace(&mut router, Query::default())
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

    TraceSpec::builder()
        .services(["router", "subgraph"].into())
        .operation_name("ExampleQuery")
        .build()
        .validate_jaeger_trace(&mut router, Query::builder().traced(false).build())
        .await?;

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

    let (_, response) = router
        .execute_query(Query::builder().traced(false).build())
        .await;
    assert!(response.headers().get("apollo-custom-trace-id").is_some());

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

    for _ in 0..100 {
        if TraceSpec::builder()
            .services(["router", "subgraph"].into())
            .operation_name("ExampleQuery")
            .build()
            .validate_jaeger_trace(&mut router, Query::builder().traced(false).build())
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

    TraceSpec::builder()
        .services(["router", "subgraph"].into())
        .build()
        .validate_jaeger_trace(&mut router, Query::builder().traced(false).build())
        .await?;
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
    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .build()
        .validate_jaeger_trace(&mut router, Query::default())
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

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .build()
        .validate_jaeger_trace(&mut router, Query::builder().build())
        .await?;
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
    TraceSpec::builder().services(["client", "router", "subgraph"].into())
        .operation_name("ExampleQuery2")
        .build()
        .validate_jaeger_trace(
        &mut router,
        Query::builder()
            .body(json!({"query":"query ExampleQuery1 {topProducts{name}}\nquery ExampleQuery2 {topProducts{name}}","variables":{}, "operationName": "ExampleQuery2"})
            ).build(),
            ).await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_span_attributes() -> Result<(), BoxError> {
    if std::env::var("TEST_APOLLO_KEY").is_ok() && std::env::var("TEST_APOLLO_GRAPH_REF").is_ok() {
        let mut router = IntegrationTest::builder()
            .telemetry(Telemetry::Jaeger)
            .config(include_str!("fixtures/jaeger-advanced.router.yaml"))
            .build()
            .await;

        router.start().await;
        router.assert_started().await;

        // attributes:
        //           http.request.method: true
        //           http.response.status_code: true
        //           url.path: true
        //           "http.request.header.x-my-header":
        //             request_header: "x-my-header"
        //           "http.request.header.x-not-present":
        //             request_header: "x-not-present"
        //             default: nope
        //           "http.request.header.x-my-header-condition":
        //             request_header: "x-my-header"
        //             condition:
        //               eq:
        //                 - request_header: "head"
        //                 - "test"
        //           studio.operation.id:
        //             studio_operation_id: true
        //       supergraph:
        //         attributes:
        //           graphql.operation.name: true
        //           graphql.operation.type: true
        //           graphql.document: true
        //       subgraph:
        //         attributes:
        //           subgraph.graphql.operation.type: true
        //           subgraph.name: true

        TraceSpec::builder()
            .services(["client", "router", "subgraph"].into())
            .operation_name("ExampleQuery")
            .span_attribute(
                "router",
                [
                    ("http.request.method", "POST"),
                    ("http.response.status_code", "200"),
                    ("url.path", "/"),
                    ("http.request.header.x-my-header", "test"),
                    ("http.request.header.x-not-present", "nope"),
                    ("http.request.header.x-my-header-condition", "test"),
                    ("studio.operation.id", "*"),
                ]
                .into(),
            )
            .span_attribute(
                "supergraph",
                [
                    ("graphql.operation.name", "ExampleQuery"),
                    ("graphql.operation.type", "query"),
                    ("graphql.document", "query ExampleQuery {topProducts{name}}"),
                ]
                .into(),
            )
            .span_attribute(
                "subgraph",
                [
                    ("subgraph.graphql.operation.type", "query"),
                    ("subgraph.name", "products"),
                ]
                .into(),
            )
            .build()
            .validate_jaeger_trace(
                &mut router,
                Query::builder()
                    .header("x-my-header", "test")
                    .header("x-my-header-condition", "condition")
                    .build(),
            )
            .await?;
        router.graceful_shutdown().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_decimal_trace_id() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/jaeger_decimal_trace_id.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (id, result) = router.execute_query(Query::default()).await;
    let id_from_router: u128 = result
        .headers()
        .get("apollo-custom-trace-id")
        .unwrap()
        .to_str()
        .unwrap_or_default()
        .parse()
        .expect("expected decimal trace ID");
    assert_eq!(format!("{id_from_router:x}"), id.to_string());
    router.graceful_shutdown().await;
    Ok(())
}

struct JaegerTraceSpec {
    trace_spec: TraceSpec,
}
impl Deref for JaegerTraceSpec {
    type Target = TraceSpec;

    fn deref(&self) -> &Self::Target {
        &self.trace_spec
    }
}

impl Verifier for JaegerTraceSpec {
    fn spec(&self) -> &TraceSpec {
        &self.trace_spec
    }

    fn verify_span_attributes(&self, trace: &Value) -> Result<(), BoxError> {
        for (span, attributes) in &self.span_attributes {
            for (key, value) in attributes {
                let binding = trace.select_path(&format!(
                    "$..spans[?(@.operationName == '{span}')]..tags..[?(@.key == '{key}')].value"
                ))?;

                let actual_value = binding
                    .first()
                    .unwrap_or_else(|| panic!("could not find attribute {key} on {span}"));
                match actual_value {
                    Value::String(_) if *value == "*" => continue,
                    Value::String(s) => {
                        assert_eq!(s, value, "unexpected attribute {key} on {span}")
                    }
                    Value::Number(_) if *value == "*" => continue,
                    Value::Number(n) => assert_eq!(
                        n.to_string(),
                        *value,
                        "unexpected attribute {key} on {span}"
                    ),
                    _ => panic!("unexpected value type"),
                }
            }
        }
        Ok(())
    }

    async fn get_trace(&self, trace_id: TraceId) -> Result<Value, BoxError> {
        let params = url::form_urlencoded::Serializer::new(String::new())
            .append_pair(
                "service",
                self.trace_spec
                    .services
                    .first()
                    .expect("expected root service"),
            )
            .finish();

        let id = trace_id.to_string();
        let url = format!("http://localhost:16686/api/traces/{id}?{params}");
        println!("url: {url}");
        let value: serde_json::Value = reqwest::get(url)
            .await
            .map_err(|e| anyhow!("failed to contact jaeger; {e}"))?
            .json()
            .await
            .map_err(|e| anyhow!("failed to contact jaeger; {e}"))?;

        Ok(value)
    }

    fn verify_version(&self, trace: &Value) -> Result<(), BoxError> {
        if let Some(expected_version) = &self.version {
            let binding = trace.select_path("$..version")?;
            let version = binding.first();
            assert_eq!(
                version
                    .expect("version expected")
                    .as_str()
                    .expect("version must be a string"),
                expected_version
            );
        }
        Ok(())
    }

    fn measured_span(&self, trace: &Value, name: &str) -> Result<bool, BoxError> {
        let binding1 = trace.select_path(&format!(
            "$..[?(@.meta.['otel.original_name'] == '{name}')].metrics.['_dd.measured']"
        ))?;
        let binding2 = trace.select_path(&format!(
            "$..[?(@.name == '{name}')].metrics.['_dd.measured']"
        ))?;
        Ok(binding1
            .first()
            .or(binding2.first())
            .and_then(|v| v.as_f64())
            .map(|v| v == 1.0)
            .unwrap_or_default())
    }

    fn verify_services(&self, trace: &Value) -> Result<(), BoxError> {
        let actual_services: HashSet<String> = trace
            .select_path("$..serviceName")?
            .into_iter()
            .filter_map(|service| service.as_string())
            .collect();
        tracing::debug!("found services {:?}", actual_services);

        let expected_services = self
            .trace_spec
            .services
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

    fn verify_spans_present(&self, trace: &Value) -> Result<(), BoxError> {
        let operation_names: HashSet<String> = trace
            .select_path("$..operationName")?
            .into_iter()
            .filter_map(|span_name| span_name.as_string())
            .collect();

        let mut span_names: HashSet<&str> = self.span_names.clone();
        if self.services.contains(&"client") {
            span_names.insert("client_request");
        }
        tracing::debug!("found spans {:?}", operation_names);
        let missing_operation_names: Vec<_> = span_names
            .iter()
            .filter(|o| !operation_names.contains(**o))
            .collect();
        if !missing_operation_names.is_empty() {
            return Err(BoxError::from(format!(
                "spans did not match, got {operation_names:?}, missing {missing_operation_names:?}"
            )));
        }
        Ok(())
    }

    fn validate_span_kind(&self, _trace: &Value, _name: &str, _kind: &str) -> Result<(), BoxError> {
        Ok(())
    }

    fn verify_operation_name(&self, trace: &Value) -> Result<(), BoxError> {
        if let Some(expected_operation_name) = &self.operation_name {
            let binding =
                trace.select_path("$..spans[?(@.operationName == 'supergraph')]..tags[?(@.key == 'graphql.operation.name')].value")?;
            let operation_name = binding.first();
            if operation_name.is_none() {
                return Err(BoxError::from("graphql.operation.name not found"));
            }
            assert_eq!(
                operation_name
                    .expect("graphql.operation.name expected")
                    .as_str()
                    .expect("graphql.operation.name must be a string"),
                expected_operation_name
            );
        }
        Ok(())
    }

    fn verify_priority_sampled(&self, trace: &Value) -> Result<(), BoxError> {
        if let Some(psr) = self.priority_sampled {
            let binding =
                trace.select_path("$..[?(@.service=='router')].metrics._sampling_priority_v1")?;
            if binding.is_empty() {
                return Err(BoxError::from("missing sampling priority"));
            }
            for sampling_priority in binding {
                assert_eq!(
                    sampling_priority
                        .as_f64()
                        .expect("psr not string")
                        .to_string(),
                    psr
                );
            }
        }
        Ok(())
    }
}

impl TraceSpec {
    async fn validate_jaeger_trace(
        self,
        router: &mut IntegrationTest,
        query: Query,
    ) -> Result<(), BoxError> {
        JaegerTraceSpec { trace_spec: self }
            .validate_trace(router, query)
            .await
    }
}
