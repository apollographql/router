extern crate core;

use std::collections::HashSet;
use std::ops::Deref;

use anyhow::anyhow;
use opentelemetry_api::trace::TraceId;
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
        let url = format!("http://localhost:9411/api/v2/trace/{id}?{params}");
        println!("url: {url}");
        let value: serde_json::Value = reqwest::get(url)
            .await
            .map_err(|e| anyhow!("failed to contact datadog; {e}"))?
            .json()
            .await
            .map_err(|e| anyhow!("failed to contact datadog; {e}"))?;
        Ok(value)
    }

    fn spec(&self) -> &TraceSpec {
        &self.trace_spec
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
