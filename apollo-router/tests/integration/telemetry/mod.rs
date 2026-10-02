use std::collections::HashMap;
use std::collections::HashSet;

use opentelemetry::trace::TraceId;
use opentelemetry_sdk::trace::IdGenerator;
use opentelemetry_sdk::trace::RandomIdGenerator;

mod apollo_otel_metrics;
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
mod datadog;
mod events;
mod logging;
mod metrics;
mod otlp;
mod propagation;
mod verifier;

/// Generates a fresh, random, valid W3C trace ID (32 lowercase hex chars) for embedding in a
/// test's `traceparent` header.
///
/// Prefer this over hand-crafting/incrementing a hex string: a test that makes several
/// trace-context-propagation requests against the same long-lived router/mock-server pair
/// needs a distinct trace ID per request, since trace lookups (`OtlpTraceSpec::get_trace`,
/// the Datadog equivalent) match on trace ID alone - reusing one means a later request's
/// span search also picks up an earlier request's spans. See the regression this fixed in
/// `test_otlp_request_with_trace_context_propagator_with_datadog` (PR #9787): three scenarios
/// shared the same hand-copied trace ID, and the fix was reincrementing hex digits by hand -
/// the same mistake this function exists to make impossible.
pub(crate) fn unique_trace_id() -> String {
    RandomIdGenerator::default().new_trace_id().to_string()
}

struct TraceSpec {
    operation_name: Option<String>,
    version: Option<String>,
    services: Vec<&'static str>,
    span_names: HashSet<&'static str>,
    measured_spans: HashSet<&'static str>,
    unmeasured_spans: HashSet<&'static str>,
    priority_sampled: Option<&'static str>,
    subgraph_sampled: Option<bool>,
    trace_id: Option<String>,
    resources: HashMap<&'static str, &'static str>,
    attributes: HashMap<&'static str, &'static str>,
}

#[buildstructor::buildstructor]
impl TraceSpec {
    #[builder]
    pub fn new(
        operation_name: Option<String>,
        version: Option<String>,
        services: Vec<&'static str>,
        span_names: HashSet<&'static str>,
        measured_spans: HashSet<&'static str>,
        unmeasured_spans: HashSet<&'static str>,
        priority_sampled: Option<&'static str>,
        subgraph_sampled: Option<bool>,
        trace_id: Option<String>,
        resources: HashMap<&'static str, &'static str>,
        attributes: HashMap<&'static str, &'static str>,
    ) -> Self {
        Self {
            operation_name,
            version,
            services,
            span_names,
            measured_spans,
            unmeasured_spans,
            priority_sampled,
            subgraph_sampled,
            trace_id,
            resources,
            attributes,
        }
    }
}

#[allow(dead_code)]
pub trait DatadogId {
    fn to_datadog(&self) -> u64;
}
impl DatadogId for TraceId {
    fn to_datadog(&self) -> u64 {
        let bytes = &self.to_bytes()[std::mem::size_of::<u64>()..std::mem::size_of::<u128>()];
        u64::from_be_bytes(bytes.try_into().unwrap())
    }
}
