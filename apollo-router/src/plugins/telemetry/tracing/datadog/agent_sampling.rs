use opentelemetry::trace::Link;
use opentelemetry::trace::SamplingDecision;
use opentelemetry::trace::SamplingResult;
use opentelemetry::trace::SpanKind;
use opentelemetry::trace::TraceId;
use opentelemetry::KeyValue;
use opentelemetry_datadog::DatadogTraceState;
use opentelemetry_sdk::trace::ShouldSample;

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
                if !self.parent_based_sampler {
                    result.trace_state = result.trace_state.with_priority_sampling(false)
                }
            }
            SamplingDecision::RecordAndSample => {
                if !self.parent_based_sampler {
                    result.trace_state = result.trace_state.with_priority_sampling(true)
                }
            }
        }

        // We always want to measure
        result.trace_state = result.trace_state.with_measuring(true);

        result
    }
}
#[cfg(test)]
mod tests {
    use buildstructor::Builder;
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
    use opentelemetry::Context;
    use opentelemetry::KeyValue;
    use opentelemetry_datadog::DatadogTraceState;
    use opentelemetry_sdk::trace::Sampler;
    use opentelemetry_sdk::trace::ShouldSample;

    use crate::plugins::telemetry::tracing::datadog::DatadogAgentSampling;

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
        assert!(!result.trace_state.priority_sampling_enabled());

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
        assert!(!result.trace_state.priority_sampling_enabled());

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
        assert!(result.trace_state.priority_sampling_enabled());

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
            Some(&Context::new().with_remote_span_context(SpanContext::new(
                TraceId::from_u128(1),
                SpanId::from_u64(1),
                TraceFlags::SAMPLED,
                true,
                // The parent is expected to set priority sampling if it wants to sample this span
                TraceState::default().with_priority_sampling(true),
            ))),
            TraceId::from_u128(1),
            "test_span",
            &SpanKind::Internal,
            &[],
            &[],
        );

        // Record and sample should remain as record and sample
        assert_eq!(result.decision, SamplingDecision::RecordAndSample);

        // Verify that the sampling priority is set to AutoKeep so the trace will be transmitted to Datadog
        assert!(result.trace_state.priority_sampling_enabled());

        // Verify that measuring is enabled
        assert!(result.trace_state.measuring_enabled());
    }
}
