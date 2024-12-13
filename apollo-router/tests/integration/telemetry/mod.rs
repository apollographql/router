use std::collections::{HashMap, HashSet};

#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
mod datadog;
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
mod jaeger;
mod logging;
mod metrics;
mod otlp;
mod propagation;
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
mod zipkin;
mod verifier;

#[derive(buildstructor::Builder)]
struct TraceSpec {
    operation_name: Option<String>,
    version: Option<String>,
    services: Vec<&'static str>,
    span_names: HashSet<&'static str>,
    measured_spans: HashSet<&'static str>,
    unmeasured_spans: HashSet<&'static str>,
    priority_sampled: Option<&'static str>,
    subgraph_sampled: Option<bool>,
    span_attributes: HashMap<&'static str, &'static str>
}


