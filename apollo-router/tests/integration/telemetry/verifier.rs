use std::time::Duration;

use anyhow::anyhow;
use opentelemetry_api::trace::SpanContext;
use opentelemetry_api::trace::TraceId;
use serde_json::Value;
use tower::BoxError;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;
use crate::integration::telemetry::TraceSpec;

pub trait Verifier {
    fn spec(&self) -> &TraceSpec;
    async fn validate_trace(
        &self,
        router: &mut IntegrationTest,
        query: Query,
    ) -> Result<(), BoxError> {
        let (id, response) = router.execute_query(query).await;
        if let Some(spec_id) = &self.spec().trace_id {
            assert_eq!(id.to_string(), *spec_id, "trace id");
        }
        for _ in 0..20 {
            if self.find_valid_trace(id).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.find_valid_trace(id).await?;
        let subgraph_context = router.subgraph_context();
        assert!(response.status().is_success());
        self.validate_subgraph(subgraph_context)?;
        Ok(())
    }

    async fn validate_metrics(&self) -> Result<(), BoxError> {
        for _ in 0..10 {
            if self.find_valid_metrics().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.find_valid_metrics().await?;
        Ok(())
    }

    async fn find_valid_metrics(&self) -> Result<(), BoxError> {
        unimplemented!("find_valid_metrics")
    }

    fn validate_subgraph(&self, subgraph_context: SpanContext) -> Result<(), BoxError> {
        self.validate_subgraph_priority_sampled(&subgraph_context)?;
        self.validate_subgraph_sampled(&subgraph_context)?;
        Ok(())
    }
    fn validate_subgraph_sampled(&self, subgraph_context: &SpanContext) -> Result<(), BoxError> {
        if let Some(sampled) = self.spec().priority_sampled {
            assert_eq!(
                subgraph_context.trace_state().get("psr"),
                Some(sampled),
                "subgraph psr"
            );
        }

        Ok(())
    }

    fn validate_subgraph_priority_sampled(
        &self,
        subgraph_context: &SpanContext,
    ) -> Result<(), BoxError> {
        if let Some(sampled) = self.spec().subgraph_sampled {
            assert_eq!(subgraph_context.is_sampled(), sampled, "subgraph sampled");
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn find_valid_trace(&self, trace_id: TraceId) -> Result<(), BoxError> {
        // A valid trace has:
        // * All three services
        // * The correct spans
        // * All spans are parented
        // * Required attributes of 'router' span has been set

        // For now just validate service name.
        let trace: Value = self.get_trace(trace_id).await?;
        println!("trace: {trace_id}");
        self.verify_services(&trace)?;
        println!("services verified");
        self.verify_spans_present(&trace)?;
        println!("spans present verified");
        self.verify_measured_spans(&trace)?;
        println!("measured spans verified");
        self.verify_operation_name(&trace)?;
        println!("operation name verified");
        self.verify_priority_sampled(&trace)?;
        println!("priority sampled verified");
        self.verify_version(&trace)?;
        println!("version verified");
        self.verify_span_kinds(&trace)?;
        println!("span kinds verified");
        self.verify_span_attributes(&trace)?;
        println!("span attributes verified");
        Ok(())
    }

    async fn get_trace(&self, trace_id: TraceId) -> Result<Value, BoxError>;

    fn verify_version(&self, trace: &Value) -> Result<(), BoxError>;

    fn verify_measured_spans(&self, trace: &Value) -> Result<(), BoxError> {
        for expected in &self.spec().measured_spans {
            let measured = self.measured_span(trace, expected)?;
            if !measured {
                return Err(anyhow!("missing measured span {expected}").into());
            }
        }
        for unexpected in &self.spec().unmeasured_spans {
            let measured = self.measured_span(trace, unexpected)?;
            if measured {
                return Err(anyhow!("unexpected measured span {measured}").into());
            }
        }
        Ok(())
    }

    fn measured_span(&self, trace: &Value, name: &str) -> Result<bool, BoxError>;

    fn verify_span_kinds(&self, trace: &Value) -> Result<(), BoxError> {
        // Validate that the span.kind has been propagated. We can just do this for a selection of spans.
        if self.spec().span_names.contains("router") {
            self.validate_span_kind(trace, "router", "server")?;
        }

        if self.spec().span_names.contains("supergraph") {
            self.validate_span_kind(trace, "supergraph", "internal")?;
        }

        if self.spec().span_names.contains("http_request") {
            self.validate_span_kind(trace, "http_request", "client")?;
        }

        Ok(())
    }

    fn verify_services(&self, trace: &Value) -> Result<(), BoxError>;

    fn verify_spans_present(&self, trace: &Value) -> Result<(), BoxError>;

    fn validate_span_kind(&self, trace: &Value, name: &str, kind: &str) -> Result<(), BoxError>;

    fn verify_span_attributes(&self, trace: &Value) -> Result<(), BoxError>;

    fn verify_operation_name(&self, trace: &Value) -> Result<(), BoxError>;

    fn verify_priority_sampled(&self, trace: &Value) -> Result<(), BoxError>;
}
