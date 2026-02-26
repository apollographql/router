extern crate core;

use std::collections::HashSet;
use std::ops::Deref;

use anyhow::anyhow;
use opentelemetry::trace::TraceId;
use serde_json::Value;
use tower::BoxError;

use crate::integration::IntegrationTest;
use crate::integration::ValueExt;
use crate::integration::common::Query;
use crate::integration::common::Telemetry;
use crate::integration::telemetry::TraceSpec;
use crate::integration::telemetry::verifier::Verifier;

#[tokio::test(flavor = "multi_thread")]
async fn test_basic() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Zipkin)
        .config(include_str!("fixtures/zipkin.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    for _ in 0..2 {
        TraceSpec::builder()
            .services(["client", "router", "subgraph"].into())
            .operation_name("ExampleQuery")
            .build()
            .validate_zipkin_trace(&mut router, Query::default())
            .await?;
        router.touch_config().await;
        router.assert_reloaded().await;
    }
    router.graceful_shutdown().await;
    Ok(())
}

struct ZipkinTraceSpec {
    trace_spec: TraceSpec,
}
impl Deref for ZipkinTraceSpec {
    type Target = TraceSpec;

    fn deref(&self) -> &Self::Target {
        &self.trace_spec
    }
}

impl Verifier for ZipkinTraceSpec {
    fn verify_span_attributes(&self, _trace: &Value) -> Result<(), BoxError> {
        Ok(())
    }
    fn verify_version(&self, _trace: &Value) -> Result<(), BoxError> {
        Ok(())
    }

    fn measured_span(&self, _trace: &Value, _name: &str) -> Result<bool, BoxError> {
        Ok(true)
    }

    fn verify_span_kinds(&self, _trace: &Value) -> Result<(), BoxError> {
        Ok(())
    }

    fn verify_services(&self, trace: &Value) -> Result<(), axum::BoxError> {
        // Note: opentelemetry-zipkin 0.31 has a bug where it doesn't set localEndpoint.serviceName
        // from the Resource. Instead of checking service names, we verify that spans exist
        // by checking for expected span names.
        // See: https://github.com/open-telemetry/opentelemetry-rust-contrib/issues/XXX
        let span_names: HashSet<String> = trace
            .select_path("$..name")?
            .into_iter()
            .filter_map(|name| name.as_string())
            .collect();
        tracing::debug!("found span names {:?}", span_names);

        // Verify we have spans from client, router, and subgraph by checking for characteristic span names
        let has_client_span = span_names.iter().any(|n| n == "client_request");
        let has_router_span = span_names
            .iter()
            .any(|n| n == "router" || n == "supergraph");
        let has_subgraph_span = span_names
            .iter()
            .any(|n| n == "subgraph server" || n.starts_with("subgraph"));

        if !has_client_span || !has_router_span || !has_subgraph_span {
            return Err(BoxError::from(format!(
                "incomplete traces, expected spans from client/router/subgraph, got span names: {span_names:?}"
            )));
        }
        Ok(())
    }

    fn verify_spans_present(&self, _trace: &Value) -> Result<(), BoxError> {
        Ok(())
    }

    fn validate_span_kind(&self, _trace: &Value, _name: &str, _kind: &str) -> Result<(), BoxError> {
        Ok(())
    }

    fn verify_operation_name(&self, trace: &Value) -> Result<(), BoxError> {
        if let Some(expected_operation_name) = &self.operation_name {
            let binding = trace
                .select_path("$..[?(@.name == 'supergraph')].tags..['graphql.operation.name']")?;
            let operation_name = binding.first();
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

    fn verify_priority_sampled(&self, _trace: &Value) -> Result<(), BoxError> {
        Ok(())
    }

    async fn get_trace(&self, trace_id: TraceId) -> Result<Value, BoxError> {
        let id = trace_id.to_string();
        let url = format!("http://localhost:9411/api/v2/trace/{id}");
        println!("url: {}", url);
        let value: serde_json::Value = reqwest::get(url)
            .await
            .map_err(|e| anyhow!("failed to contact zipkin; {}", e))?
            .json()
            .await
            .map_err(|e| anyhow!("failed to contact zipkin; {}", e))?;
        Ok(value)
    }

    fn spec(&self) -> &TraceSpec {
        &self.trace_spec
    }

    fn verify_resources(&self, _trace: &Value) -> Result<(), BoxError> {
        Ok(())
    }
}

impl TraceSpec {
    async fn validate_zipkin_trace(
        self,
        router: &mut IntegrationTest,
        query: Query,
    ) -> Result<(), BoxError> {
        ZipkinTraceSpec { trace_spec: self }
            .validate_trace(router, query)
            .await
    }
}
