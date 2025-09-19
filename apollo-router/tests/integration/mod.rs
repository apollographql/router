mod batching;
#[path = "../common.rs"]
pub(crate) mod common;
pub(crate) use common::IntegrationTest;

mod allowed_features;
mod connectors;
mod coprocessor;
mod docs;
// In the CI environment we only install Redis on x86_64 Linux
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
mod entity_cache;
mod file_upload;
mod introspection;
mod lifecycle;
mod metrics;
mod mock_subgraphs;
mod oci;
mod operation_limits;
mod operation_name;
mod query_planner;
mod subgraph_response;
mod supergraph;
mod traffic_shaping;
mod typename;

// In the CI environment we only install Redis on x86_64 Linux
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
mod redis_response_cache;
// In the CI environment we only install Redis on x86_64 Linux
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
mod response_cache;
// In the CI environment we only install Redis on x86_64 Linux
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
mod redis;
mod rhai;
mod subscription_load_test;
mod subscriptions;
mod telemetry;
mod validation;

use jsonpath_lib::Selector;
use serde_json::Value;
use tower::BoxError;

pub trait ValueExt {
    fn select_path<'a>(&'a self, path: &str) -> Result<Vec<&'a Value>, BoxError>;
    fn as_string(&self) -> Option<String>;
}

impl ValueExt for Value {
    fn select_path<'a>(&'a self, path: &str) -> Result<Vec<&'a Value>, BoxError> {
        Ok(Selector::new().str_path(path)?.value(self).select()?)
    }
    fn as_string(&self) -> Option<String> {
        self.as_str().map(|s| s.to_string())
    }
}

impl ValueExt for &Value {
    fn select_path<'a>(&'a self, path: &str) -> Result<Vec<&'a Value>, BoxError> {
        Ok(Selector::new().str_path(path)?.value(self).select()?)
    }
    fn as_string(&self) -> Option<String> {
        self.as_str().map(|s| s.to_string())
    }
}
