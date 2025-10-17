use std::collections::HashMap;
use std::collections::HashSet;

use opentelemetry::trace::TraceId;

mod apollo_otel_metrics;
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
mod datadog;
mod events;
mod logging;
mod metrics;
mod otlp;
mod propagation;
mod verifier;
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
mod zipkin;

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
