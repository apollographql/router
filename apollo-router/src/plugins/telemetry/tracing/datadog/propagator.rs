//! Datadog propagator implementation with full SamplingPriority support.
//!
//! This is kept locally rather than using opentelemetry-datadog's propagator
//! because we need the full SamplingPriority enum (UserReject/AutoReject/AutoKeep/UserKeep)
//! rather than just a boolean flag.

use std::fmt::Display;

use once_cell::sync::Lazy;
use opentelemetry::propagation::text_map_propagator::FieldIter;
use opentelemetry::propagation::Extractor;
use opentelemetry::propagation::Injector;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::SpanId;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TraceFlags;
use opentelemetry::trace::TraceId;
use opentelemetry::trace::TraceState;
use opentelemetry::Context;

const DATADOG_TRACE_ID_HEADER: &str = "x-datadog-trace-id";
const DATADOG_PARENT_ID_HEADER: &str = "x-datadog-parent-id";
const DATADOG_SAMPLING_PRIORITY_HEADER: &str = "x-datadog-sampling-priority";

const TRACE_FLAG_DEFERRED: TraceFlags = TraceFlags::new(0x02);
const TRACE_STATE_PRIORITY_SAMPLING: &str = "psr";
const TRACE_STATE_MEASURE: &str = "m";
const TRACE_STATE_TRUE_VALUE: &str = "1";
const TRACE_STATE_FALSE_VALUE: &str = "0";

static DATADOG_HEADER_FIELDS: Lazy<[String; 3]> = Lazy::new(|| {
    [
        DATADOG_TRACE_ID_HEADER.to_string(),
        DATADOG_PARENT_ID_HEADER.to_string(),
        DATADOG_SAMPLING_PRIORITY_HEADER.to_string(),
    ]
});

/// Builder for constructing Datadog trace state.
/// Used in tests to create expected SpanContext values.
#[derive(Default)]
#[cfg(test)]
struct DatadogTraceStateBuilder {
    sampling_priority: SamplingPriority,
    measuring: Option<bool>,
}

fn boolean_to_trace_state_flag(value: bool) -> &'static str {
    if value {
        TRACE_STATE_TRUE_VALUE
    } else {
        TRACE_STATE_FALSE_VALUE
    }
}

fn trace_flag_to_boolean(value: &str) -> bool {
    value == TRACE_STATE_TRUE_VALUE
}

#[cfg(test)]
#[allow(clippy::needless_update)]
impl DatadogTraceStateBuilder {
    pub fn with_priority_sampling(self, sampling_priority: SamplingPriority) -> Self {
        Self {
            sampling_priority,
            ..self
        }
    }

    pub fn with_measuring(self, enabled: bool) -> Self {
        Self {
            measuring: Some(enabled),
            ..self
        }
    }

    pub fn build(self) -> TraceState {
        if let Some(measuring) = self.measuring {
            let values = [
                (TRACE_STATE_MEASURE, boolean_to_trace_state_flag(measuring)),
                (
                    TRACE_STATE_PRIORITY_SAMPLING,
                    &self.sampling_priority.to_string(),
                ),
            ];

            TraceState::from_key_value(values).unwrap_or_default()
        } else {
            let values = [(
                TRACE_STATE_PRIORITY_SAMPLING,
                &self.sampling_priority.to_string(),
            )];

            TraceState::from_key_value(values).unwrap_or_default()
        }
    }
}

pub(crate) trait DatadogTraceState {
    fn with_measuring(&self, enabled: bool) -> TraceState;

    fn measuring_enabled(&self) -> bool;

    fn with_priority_sampling(&self, sampling_priority: SamplingPriority) -> TraceState;

    fn sampling_priority(&self) -> Option<SamplingPriority>;
}

impl DatadogTraceState for TraceState {
    fn with_measuring(&self, enabled: bool) -> TraceState {
        self.insert(TRACE_STATE_MEASURE, boolean_to_trace_state_flag(enabled))
            .unwrap_or_else(|_err| self.clone())
    }

    fn measuring_enabled(&self) -> bool {
        self.get(TRACE_STATE_MEASURE)
            .map(trace_flag_to_boolean)
            .unwrap_or_default()
    }

    fn with_priority_sampling(&self, sampling_priority: SamplingPriority) -> TraceState {
        self.insert(TRACE_STATE_PRIORITY_SAMPLING, sampling_priority.to_string())
            .unwrap_or_else(|_err| self.clone())
    }

    fn sampling_priority(&self) -> Option<SamplingPriority> {
        self.get(TRACE_STATE_PRIORITY_SAMPLING).map(|value| {
            SamplingPriority::try_from(value).unwrap_or(SamplingPriority::AutoReject)
        })
    }
}

#[derive(Default, Debug, Eq, PartialEq, Clone, Copy)]
pub(crate) enum SamplingPriority {
    UserReject = -1,
    #[default]
    AutoReject = 0,
    AutoKeep = 1,
    UserKeep = 2,
}

impl SamplingPriority {
    pub(crate) fn as_i64(&self) -> i64 {
        match self {
            SamplingPriority::UserReject => -1,
            SamplingPriority::AutoReject => 0,
            SamplingPriority::AutoKeep => 1,
            SamplingPriority::UserKeep => 2,
        }
    }
}

impl Display for SamplingPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            SamplingPriority::UserReject => -1,
            SamplingPriority::AutoReject => 0,
            SamplingPriority::AutoKeep => 1,
            SamplingPriority::UserKeep => 2,
        };
        write!(f, "{value}")
    }
}

impl TryFrom<&str> for SamplingPriority {
    type Error = ExtractError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "-1" => Ok(SamplingPriority::UserReject),
            "0" => Ok(SamplingPriority::AutoReject),
            "1" => Ok(SamplingPriority::AutoKeep),
            "2" => Ok(SamplingPriority::UserKeep),
            _ => Err(ExtractError::SamplingPriority),
        }
    }
}

#[derive(Debug)]
pub(crate) enum ExtractError {
    TraceId,
    SpanId,
    SamplingPriority,
}

/// Extracts and injects `SpanContext`s into `Extractor`s or `Injector`s using Datadog's header format.
///
/// The Datadog header format does not have an explicit spec, but can be divined from the client libraries,
/// such as [dd-trace-go](https://github.com/DataDog/dd-trace-go/blob/v1.28.0/ddtrace/tracer/textmap.go#L293)
#[derive(Clone, Debug, Default)]
pub(crate) struct DatadogPropagator {
    _private: (),
}

fn create_trace_state_and_flags(trace_flags: TraceFlags) -> (TraceState, TraceFlags) {
    (TraceState::default(), trace_flags)
}

impl DatadogPropagator {
    fn extract_trace_id(&self, trace_id: &str) -> Result<TraceId, ExtractError> {
        trace_id
            .parse::<u64>()
            .map(|id| TraceId::from(id as u128))
            .map_err(|_| ExtractError::TraceId)
    }

    fn extract_span_id(&self, span_id: &str) -> Result<SpanId, ExtractError> {
        span_id
            .parse::<u64>()
            .map(SpanId::from)
            .map_err(|_| ExtractError::SpanId)
    }

    fn extract_span_context(&self, extractor: &dyn Extractor) -> Result<SpanContext, ExtractError> {
        let trace_id =
            self.extract_trace_id(extractor.get(DATADOG_TRACE_ID_HEADER).unwrap_or(""))?;
        // If we have a trace_id but can't get the parent span, we default it to invalid instead of completely erroring
        // out so that the rest of the spans aren't completely lost
        let span_id = self
            .extract_span_id(extractor.get(DATADOG_PARENT_ID_HEADER).unwrap_or(""))
            .unwrap_or(SpanId::INVALID);
        let sampling_priority = extractor
            .get(DATADOG_SAMPLING_PRIORITY_HEADER)
            .unwrap_or("")
            .try_into();

        let sampled = match sampling_priority {
            Ok(SamplingPriority::UserReject) | Ok(SamplingPriority::AutoReject) => {
                TraceFlags::default()
            }
            Ok(SamplingPriority::UserKeep) | Ok(SamplingPriority::AutoKeep) => TraceFlags::SAMPLED,
            // Treat the sampling as DEFERRED instead of erroring on extracting the span context
            Err(_) => TRACE_FLAG_DEFERRED,
        };

        let (mut trace_state, trace_flags) = create_trace_state_and_flags(sampled);
        if let Ok(sampling_priority) = sampling_priority {
            trace_state = trace_state.with_priority_sampling(sampling_priority);
        }

        Ok(SpanContext::new(
            trace_id,
            span_id,
            trace_flags,
            true,
            trace_state,
        ))
    }
}

impl TextMapPropagator for DatadogPropagator {
    fn inject_context(&self, cx: &Context, injector: &mut dyn Injector) {
        let span = cx.span();
        let span_context = span.span_context();
        if span_context.is_valid() {
            injector.set(
                DATADOG_TRACE_ID_HEADER,
                (u128::from_be_bytes(span_context.trace_id().to_bytes()) as u64).to_string(),
            );
            injector.set(
                DATADOG_PARENT_ID_HEADER,
                u64::from_be_bytes(span_context.span_id().to_bytes()).to_string(),
            );

            if span_context.trace_flags() & TRACE_FLAG_DEFERRED != TRACE_FLAG_DEFERRED {
                // The sampling priority
                let sampling_priority = span_context
                    .trace_state()
                    .sampling_priority()
                    .unwrap_or_else(|| {
                        if span_context.is_sampled() {
                            SamplingPriority::AutoKeep
                        } else {
                            SamplingPriority::AutoReject
                        }
                    });
                injector.set(
                    DATADOG_SAMPLING_PRIORITY_HEADER,
                    (sampling_priority as i32).to_string(),
                );
            }
        }
    }

    fn extract_with_context(&self, cx: &Context, extractor: &dyn Extractor) -> Context {
        self.extract_span_context(extractor)
            .map(|sc| cx.with_remote_span_context(sc))
            .unwrap_or_else(|_| cx.clone())
    }

    fn fields(&self) -> FieldIter<'_> {
        FieldIter::new(DATADOG_HEADER_FIELDS.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use opentelemetry::trace::TraceState;
    use opentelemetry_sdk::testing::trace::TestSpan;

    use super::*;

    #[rustfmt::skip]
    fn extract_test_data() -> Vec<(Vec<(&'static str, &'static str)>, SpanContext)> {
        vec![
            (vec![], SpanContext::empty_context()),
            (vec![(DATADOG_SAMPLING_PRIORITY_HEADER, "0")], SpanContext::empty_context()),
            (vec![(DATADOG_TRACE_ID_HEADER, "garbage")], SpanContext::empty_context()),
            (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "garbage")], SpanContext::new(TraceId::from(1234), SpanId::INVALID, TRACE_FLAG_DEFERRED, true, TraceState::default())),
            (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "12")], SpanContext::new(TraceId::from(1234), SpanId::from(12), TRACE_FLAG_DEFERRED, true, TraceState::default())),
            (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "12"), (DATADOG_SAMPLING_PRIORITY_HEADER, "-1")], SpanContext::new(TraceId::from(1234), SpanId::from(12), TraceFlags::default(), true, DatadogTraceStateBuilder::default().with_priority_sampling(SamplingPriority::UserReject).build())),
            (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "12"), (DATADOG_SAMPLING_PRIORITY_HEADER, "0")], SpanContext::new(TraceId::from(1234), SpanId::from(12), TraceFlags::default(), true, DatadogTraceStateBuilder::default().with_priority_sampling(SamplingPriority::AutoReject).build())),
            (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "12"), (DATADOG_SAMPLING_PRIORITY_HEADER, "1")], SpanContext::new(TraceId::from(1234), SpanId::from(12), TraceFlags::SAMPLED, true, DatadogTraceStateBuilder::default().with_priority_sampling(SamplingPriority::AutoKeep).build())),
            (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "12"), (DATADOG_SAMPLING_PRIORITY_HEADER, "2")], SpanContext::new(TraceId::from(1234), SpanId::from(12), TraceFlags::SAMPLED, true, DatadogTraceStateBuilder::default().with_priority_sampling(SamplingPriority::UserKeep).build())),
        ]
    }

    #[rustfmt::skip]
    fn inject_test_data() -> Vec<(Vec<(&'static str, &'static str)>, SpanContext)> {
        vec![
            (vec![], SpanContext::empty_context()),
            (vec![], SpanContext::new(TraceId::INVALID, SpanId::INVALID, TRACE_FLAG_DEFERRED, true, TraceState::default())),
            (vec![], SpanContext::new(TraceId::from_hex("1234").unwrap(), SpanId::INVALID, TRACE_FLAG_DEFERRED, true, TraceState::default())),
            (vec![], SpanContext::new(TraceId::from_hex("1234").unwrap(), SpanId::INVALID, TraceFlags::SAMPLED, true, TraceState::default())),
            (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "12")], SpanContext::new(TraceId::from(1234), SpanId::from(12), TRACE_FLAG_DEFERRED, true, TraceState::default())),
            (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "12"), (DATADOG_SAMPLING_PRIORITY_HEADER, "-1")], SpanContext::new(TraceId::from(1234), SpanId::from(12), TraceFlags::default(), true, DatadogTraceStateBuilder::default().with_priority_sampling(SamplingPriority::UserReject).build())),
            (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "12"), (DATADOG_SAMPLING_PRIORITY_HEADER, "0")], SpanContext::new(TraceId::from(1234), SpanId::from(12), TraceFlags::default(), true, DatadogTraceStateBuilder::default().with_priority_sampling(SamplingPriority::AutoReject).build())),
            (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "12"), (DATADOG_SAMPLING_PRIORITY_HEADER, "1")], SpanContext::new(TraceId::from(1234), SpanId::from(12), TraceFlags::SAMPLED, true, DatadogTraceStateBuilder::default().with_priority_sampling(SamplingPriority::AutoKeep).build())),
            (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "12"), (DATADOG_SAMPLING_PRIORITY_HEADER, "2")], SpanContext::new(TraceId::from(1234), SpanId::from(12), TraceFlags::SAMPLED, true, DatadogTraceStateBuilder::default().with_priority_sampling(SamplingPriority::UserKeep).build())),
        ]
    }

    #[test]
    fn test_extract() {
        for (header_list, expected) in extract_test_data() {
            let map: HashMap<String, String> = header_list
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();

            let propagator = DatadogPropagator::default();
            let context = propagator.extract(&map);
            assert_eq!(context.span().span_context(), &expected);
        }
    }

    #[test]
    fn test_extract_empty() {
        let map: HashMap<String, String> = HashMap::new();
        let propagator = DatadogPropagator::default();
        let context = propagator.extract(&map);
        assert_eq!(context.span().span_context(), &SpanContext::empty_context())
    }

    #[test]
    fn test_extract_with_empty_remote_context() {
        let map: HashMap<String, String> = HashMap::new();
        let propagator = DatadogPropagator::default();
        let context = propagator.extract_with_context(&Context::new(), &map);
        assert!(!context.has_active_span())
    }

    #[test]
    fn test_inject() {
        let propagator = DatadogPropagator::default();
        for (header_values, span_context) in inject_test_data() {
            let mut injector: HashMap<String, String> = HashMap::new();
            propagator.inject_context(
                &Context::current_with_span(TestSpan(span_context)),
                &mut injector,
            );

            if !header_values.is_empty() {
                for (k, v) in header_values.into_iter() {
                    let injected_value: Option<&String> = injector.get(k);
                    assert_eq!(injected_value, Some(&v.to_string()));
                    injector.remove(k);
                }
            }
            assert!(injector.is_empty());
        }
    }
}
