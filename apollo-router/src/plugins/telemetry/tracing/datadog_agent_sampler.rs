use opentelemetry_api::trace::Link;
use opentelemetry_api::trace::SamplingDecision;
use opentelemetry_api::trace::SamplingResult;
use opentelemetry_api::trace::SpanKind;
use opentelemetry_api::trace::TraceId;
use opentelemetry_api::Key;
use opentelemetry_api::KeyValue;
use opentelemetry_api::OrderMap;
use opentelemetry_api::Value;
use opentelemetry_sdk::trace::ShouldSample;

use crate::plugins::telemetry::tracing::datadog_exporter::propagator::SamplingPriority;
use crate::plugins::telemetry::tracing::datadog_exporter::DatadogTraceState;

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
    pub(crate) sampler: opentelemetry::sdk::trace::Sampler,
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
        sampler: opentelemetry::sdk::trace::Sampler,
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
        parent_context: Option<&opentelemetry_api::Context>,
        trace_id: TraceId,
        name: &str,
        span_kind: &SpanKind,
        attributes: &OrderMap<Key, Value>,
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
        result.attributes.push(KeyValue::new(
            "sampling.priority",
            Value::I64(
                result
                    .trace_state
                    .sampling_priority()
                    .expect("sampling priority")
                    .as_i64(),
            ),
        ));
        result
    }
}
#[cfg(test)]
mod tests {
    use opentelemetry::sdk::trace::Sampler;
    use opentelemetry::trace::TraceState;
    use opentelemetry_api::trace::Link;
    use opentelemetry_api::trace::SamplingDecision;
    use opentelemetry_api::trace::SamplingResult;
    use opentelemetry_api::trace::SpanKind;
    use opentelemetry_api::trace::TraceId;
    use opentelemetry_api::Context;
    use opentelemetry_api::Key;
    use opentelemetry_api::OrderMap;
    use opentelemetry_api::Value;
    use opentelemetry_sdk::trace::ShouldSample;

    use crate::plugins::telemetry::tracing::datadog_agent_sampler::DatadogAgentSampling;
    use crate::plugins::telemetry::tracing::datadog_exporter::propagator::SamplingPriority;
    use crate::plugins::telemetry::tracing::datadog_exporter::DatadogTraceState;

    #[derive(Debug, Clone)]
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
            _attributes: &OrderMap<Key, Value>,
            _links: &[Link],
        ) -> SamplingResult {
            SamplingResult {
                decision: self.decision.clone(),
                attributes: Vec::new(),
                trace_state: TraceState::default(),
            }
        }
    }

    #[test]
    fn test_should_sample_drop() {
        // Test case where the sampling decision is Drop
        let sampler = StubSampler {
            decision: SamplingDecision::Drop,
        };
        let datadog_sampler =
            DatadogAgentSampling::new(Sampler::ParentBased(Box::new(sampler)), false);

        let result = datadog_sampler.should_sample(
            None,
            TraceId::from_u128(1),
            "test_span",
            &SpanKind::Internal,
            &OrderMap::new(),
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
        assert!(result
            .attributes
            .iter()
            .any(|kv| kv.key.as_str() == "sampling.priority"
                && kv.value == Value::I64(SamplingPriority::AutoReject.as_i64())));
    }

    #[test]
    fn test_should_sample_record_only() {
        let sampler = StubSampler {
            decision: SamplingDecision::RecordOnly,
        };
        let datadog_sampler =
            DatadogAgentSampling::new(Sampler::ParentBased(Box::new(sampler)), false);

        let result = datadog_sampler.should_sample(
            None,
            TraceId::from_u128(1),
            "test_span",
            &SpanKind::Internal,
            &OrderMap::new(),
            &[],
        );

        assert_eq!(result.decision, SamplingDecision::RecordOnly);
        assert_eq!(
            result.trace_state.sampling_priority(),
            Some(SamplingPriority::AutoReject)
        );
        assert!(result
            .attributes
            .iter()
            .any(|kv| kv.key.as_str() == "sampling.priority"
                && kv.value == Value::I64(SamplingPriority::AutoReject.as_i64())));
    }

    #[test]
    fn test_should_sample_record_and_sample() {
        let sampler = StubSampler {
            decision: SamplingDecision::RecordAndSample,
        };
        let datadog_sampler =
            DatadogAgentSampling::new(Sampler::ParentBased(Box::new(sampler)), false);

        let result = datadog_sampler.should_sample(
            None,
            TraceId::from_u128(1),
            "test_span",
            &SpanKind::Internal,
            &OrderMap::new(),
            &[],
        );

        assert_eq!(result.decision, SamplingDecision::RecordAndSample);
        assert_eq!(
            result.trace_state.sampling_priority(),
            Some(SamplingPriority::AutoKeep)
        );
        assert!(result
            .attributes
            .iter()
            .any(|kv| kv.key.as_str() == "sampling.priority"
                && kv.value == Value::I64(SamplingPriority::AutoKeep.as_i64())));
    }

    #[test]
    fn test_should_sample_with_parent_based_sampler() {
        let sampler = StubSampler {
            decision: SamplingDecision::RecordAndSample,
        };
        let datadog_sampler =
            DatadogAgentSampling::new(Sampler::ParentBased(Box::new(sampler)), true);

        let result = datadog_sampler.should_sample(
            None,
            TraceId::from_u128(1),
            "test_span",
            &SpanKind::Internal,
            &OrderMap::new(),
            &[],
        );

        assert_eq!(result.decision, SamplingDecision::RecordAndSample);
        assert_eq!(
            result.trace_state.sampling_priority(),
            Some(SamplingPriority::AutoKeep)
        );
        assert!(result
            .attributes
            .iter()
            .any(|kv| kv.key.as_str() == "sampling.priority"
                && kv.value == Value::I64(SamplingPriority::AutoKeep.as_i64())));
    }
}
