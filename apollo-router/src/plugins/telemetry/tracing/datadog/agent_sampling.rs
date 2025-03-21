use opentelemetry::KeyValue;
use opentelemetry::Value;
use opentelemetry::trace::Link;
use opentelemetry::trace::SamplingDecision;
use opentelemetry::trace::SamplingResult;
use opentelemetry::trace::SpanKind;
use opentelemetry::trace::TraceId;
use opentelemetry_sdk::trace::ShouldSample;

use crate::plugins::telemetry::tracing::datadog_exporter::DatadogTraceState;
use crate::plugins::telemetry::tracing::datadog_exporter::propagator::SamplingPriority;

/// The Datadog Agent Sampler
///
/// This sampler overrides the sampling decision to ensure that spans are recorded even if they were originally dropped.
/// It performs the following tasks:
/// 1. Ensures the appropriate trace state is set
/// 2. Adds the sampling.priority attribute to the span
///
/// The sampler can be configured to use parent-based sampling for consistent trace sampling.
///
#[derive(Debug, Clone)]
pub(crate) struct DatadogAgentSampling {
    /// The underlying sampler used for initial sampling decisions
    pub(crate) sampler: opentelemetry_sdk::trace::Sampler,
    /// Flag to enable parent-based sampling for consistent trace sampling
    pub(crate) parent_based_sampler: bool,
}
impl DatadogAgentSampling {
    /// Creates a new DatadogAgentSampling instance
    ///
    /// # Arguments
    /// * `sampler` - The underlying sampler to use for initial sampling decisions
    /// * `parent_based_sampler` - Whether to use parent-based sampling for consistent trace sampling
    pub(crate) fn new(
        sampler: opentelemetry_sdk::trace::Sampler,
        parent_based_sampler: bool,
    ) -> Self {
        Self {
            sampler,
            parent_based_sampler,
        }
    }
}

impl ShouldSample for DatadogAgentSampling {
    fn should_sample(
        &self,
        parent_context: Option<&opentelemetry::Context>,
        trace_id: TraceId,
        name: &str,
        span_kind: &SpanKind,
        attributes: &[KeyValue],
        links: &[Link],
    ) -> SamplingResult {
        let mut result = self.sampler.should_sample(
            parent_context,
            trace_id,
            name,
            span_kind,
            attributes,
            links,
        );
        // Override the sampling decision to record and make sure that the trace state is set correctly
        // if either parent sampling is disabled or it has not been populated by a propagator.
        // The propagator gets first dibs on setting the trace state, so if it sets it, we don't override it unless we are not parent based.
        match result.decision {
            SamplingDecision::Drop | SamplingDecision::RecordOnly => {
                result.decision = SamplingDecision::RecordOnly;
                if !self.parent_based_sampler || result.trace_state.sampling_priority().is_none() {
                    result.trace_state = result
                        .trace_state
                        .with_priority_sampling(SamplingPriority::AutoReject)
                }
            }
            SamplingDecision::RecordAndSample => {
                if !self.parent_based_sampler || result.trace_state.sampling_priority().is_none() {
                    result.trace_state = result
                        .trace_state
                        .with_priority_sampling(SamplingPriority::AutoKeep)
                }
            }
        }

        // We always want to measure
        result.trace_state = result.trace_state.with_measuring(true);
        // We always want to set the sampling.priority attribute in case we are communicating with the agent via otlp.
        // Reverse engineered from https://github.com/DataDog/datadog-agent/blob/c692f62423f93988b008b669008f9199a5ad196b/pkg/trace/api/otlp.go#L502
        if let Some(priority) = result.trace_state.sampling_priority() {
            result.attributes.push(KeyValue::new(
                "sampling.priority",
                Value::I64(priority.as_i64()),
            ));
        } else {
            tracing::error!("Failed to set trace sampling priority.");
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use buildstructor::Builder;
    use opentelemetry::Context;
    use opentelemetry::KeyValue;
    use opentelemetry::Value;
    use opentelemetry::trace::Link;
    use opentelemetry::trace::SamplingDecision;
    use opentelemetry::trace::SamplingResult;
    use opentelemetry::trace::SpanContext;
    use opentelemetry::trace::SpanId;
    use opentelemetry::trace::SpanKind;
    use opentelemetry::trace::TraceContextExt;
    use opentelemetry::trace::TraceFlags;
    use opentelemetry::trace::TraceId;
    use opentelemetry::trace::TraceState;
    use opentelemetry_sdk::trace::Sampler;
    use opentelemetry_sdk::trace::ShouldSample;

    use crate::plugins::telemetry::tracing::datadog::DatadogAgentSampling;
    use crate::plugins::telemetry::tracing::datadog_exporter::DatadogTraceState;
    use crate::plugins::telemetry::tracing::datadog_exporter::propagator::SamplingPriority;

    #[derive(Debug, Clone, Builder)]
    struct StubSampler {
        decision: SamplingDecision,
    }

    impl ShouldSample for StubSampler {
        fn should_sample(
            &self,
            _parent_context: Option<&Context>,
            _trace_id: TraceId,
            _name: &str,
            _span_kind: &SpanKind,
            _attributes: &[KeyValue],
            _links: &[Link],
        ) -> SamplingResult {
            SamplingResult {
                decision: self.decision.clone(),
                attributes: Vec::new(),
                trace_state: Default::default(),
            }
        }
    }

    #[test]
    fn test_should_sample_drop() {
        // Test case where the sampling decision is Drop
        let sampler = StubSampler::builder()
            .decision(SamplingDecision::Drop)
            .build();
        let datadog_sampler =
            DatadogAgentSampling::new(Sampler::ParentBased(Box::new(sampler)), false);

        let result = datadog_sampler.should_sample(
            None,
            TraceId::from_u128(1),
            "test_span",
            &SpanKind::Internal,
            &[],
            &[],
        );

        // Verify that the decision is RecordOnly (converted from Drop)
        assert_eq!(result.decision, SamplingDecision::RecordOnly);
        // Verify that the sampling priority is set to AutoReject
        assert_eq!(
            result.trace_state.sampling_priority(),
            Some(SamplingPriority::AutoReject)
        );
        // Verify that the sampling.priority attribute is set correctly
        assert!(
            result
                .attributes
                .iter()
                .any(|kv| kv.key.as_str() == "sampling.priority"
                    && kv.value == Value::I64(SamplingPriority::AutoReject.as_i64()))
        );

        // Verify that measuring is enabled
        assert!(result.trace_state.measuring_enabled());
    }

    #[test]
    fn test_should_sample_record_only() {
        let sampler = StubSampler::builder()
            .decision(SamplingDecision::RecordOnly)
            .build();
        let datadog_sampler =
            DatadogAgentSampling::new(Sampler::ParentBased(Box::new(sampler)), false);

        let result = datadog_sampler.should_sample(
            None,
            TraceId::from_u128(1),
            "test_span",
            &SpanKind::Internal,
            &[],
            &[],
        );

        // Record only should remain as record only
        assert_eq!(result.decision, SamplingDecision::RecordOnly);

        // Verify that the sampling priority is set to AutoReject so the trace won't be transmitted to Datadog
        assert_eq!(
            result.trace_state.sampling_priority(),
            Some(SamplingPriority::AutoReject)
        );
        assert!(
            result
                .attributes
                .iter()
                .any(|kv| kv.key.as_str() == "sampling.priority"
                    && kv.value == Value::I64(SamplingPriority::AutoReject.as_i64()))
        );

        // Verify that measuring is enabled
        assert!(result.trace_state.measuring_enabled());
    }

    #[test]
    fn test_should_sample_record_and_sample() {
        let sampler = StubSampler::builder()
            .decision(SamplingDecision::RecordAndSample)
            .build();
        let datadog_sampler =
            DatadogAgentSampling::new(Sampler::ParentBased(Box::new(sampler)), false);

        let result = datadog_sampler.should_sample(
            None,
            TraceId::from_u128(1),
            "test_span",
            &SpanKind::Internal,
            &[],
            &[],
        );

        // Record and sample should remain as record and sample
        assert_eq!(result.decision, SamplingDecision::RecordAndSample);

        // Verify that the sampling priority is set to AutoKeep so the trace will be transmitted to Datadog
        assert_eq!(
            result.trace_state.sampling_priority(),
            Some(SamplingPriority::AutoKeep)
        );
        assert!(
            result
                .attributes
                .iter()
                .any(|kv| kv.key.as_str() == "sampling.priority"
                    && kv.value == Value::I64(SamplingPriority::AutoKeep.as_i64()))
        );

        // Verify that measuring is enabled
        assert!(result.trace_state.measuring_enabled());
    }

    #[test]
    fn test_should_sample_with_parent_based_sampler() {
        let sampler = StubSampler::builder()
            .decision(SamplingDecision::RecordAndSample)
            .build();

        let datadog_sampler =
            DatadogAgentSampling::new(Sampler::ParentBased(Box::new(sampler)), true);

        let result = datadog_sampler.should_sample(
            Some(&Context::new()),
            TraceId::from_u128(1),
            "test_span",
            &SpanKind::Internal,
            &[],
            &[],
        );

        // Record and sample should remain as record and sample
        assert_eq!(result.decision, SamplingDecision::RecordAndSample);

        // Verify that the sampling priority is set to AutoKeep so the trace will be transmitted to Datadog
        assert_eq!(
            result.trace_state.sampling_priority(),
            Some(SamplingPriority::AutoKeep)
        );
        assert!(
            result
                .attributes
                .iter()
                .any(|kv| kv.key.as_str() == "sampling.priority"
                    && kv.value == Value::I64(SamplingPriority::AutoKeep.as_i64()))
        );

        // Verify that measuring is enabled
        assert!(result.trace_state.measuring_enabled());
    }

    #[test]
    fn test_trace_state_already_populated_record_and_sample() {
        let sampler = StubSampler::builder()
            .decision(SamplingDecision::RecordAndSample)
            .build();

        let datadog_sampler =
            DatadogAgentSampling::new(Sampler::ParentBased(Box::new(sampler)), true);

        let result = datadog_sampler.should_sample(
            Some(&Context::new().with_remote_span_context(SpanContext::new(
                TraceId::from_u128(1),
                SpanId::from_u64(1),
                TraceFlags::SAMPLED,
                true,
                TraceState::default().with_priority_sampling(SamplingPriority::UserReject),
            ))),
            TraceId::from_u128(1),
            "test_span",
            &SpanKind::Internal,
            &[],
            &[],
        );

        // Record and sample should remain as record and sample
        assert_eq!(result.decision, SamplingDecision::RecordAndSample);

        // Verify that the sampling priority is not overridden
        assert_eq!(
            result.trace_state.sampling_priority(),
            Some(SamplingPriority::UserReject)
        );
        assert!(
            result
                .attributes
                .iter()
                .any(|kv| kv.key.as_str() == "sampling.priority"
                    && kv.value == Value::I64(SamplingPriority::UserReject.as_i64()))
        );

        // Verify that measuring is enabled
        assert!(result.trace_state.measuring_enabled());
    }

    #[test]
    fn test_trace_state_already_populated_record_drop() {
        let sampler = StubSampler::builder()
            .decision(SamplingDecision::Drop)
            .build();

        let datadog_sampler =
            DatadogAgentSampling::new(Sampler::ParentBased(Box::new(sampler)), true);

        let result = datadog_sampler.should_sample(
            Some(&Context::new().with_remote_span_context(SpanContext::new(
                TraceId::from_u128(1),
                SpanId::from_u64(1),
                TraceFlags::default(),
                true,
                TraceState::default().with_priority_sampling(SamplingPriority::UserReject),
            ))),
            TraceId::from_u128(1),
            "test_span",
            &SpanKind::Internal,
            &[],
            &[],
        );

        // Drop is converted to RecordOnly
        assert_eq!(result.decision, SamplingDecision::RecordOnly);

        // Verify that the sampling priority is not overridden
        assert_eq!(
            result.trace_state.sampling_priority(),
            Some(SamplingPriority::UserReject)
        );
        assert!(
            result
                .attributes
                .iter()
                .any(|kv| kv.key.as_str() == "sampling.priority"
                    && kv.value == Value::I64(SamplingPriority::UserReject.as_i64()))
        );

        // Verify that measuring is enabled
        assert!(result.trace_state.measuring_enabled());
    }
}
