mod batching;
#[path = "../common.rs"]
pub(crate) mod common;
pub(crate) use common::IntegrationTest;

#[cfg(feature = "test-jwks")]
mod allowed_features;

#[cfg(not(feature = "test-jwks"))]
mod connectors;
#[cfg(not(feature = "test-jwks"))]
mod coprocessor;
#[cfg(not(feature = "test-jwks"))]
mod docs;
// In the CI environment we only install Redis on x86_64 Linux
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
mod entity_cache;
#[cfg(not(feature = "test-jwks"))]
mod file_upload;
#[cfg(not(feature = "test-jwks"))]
mod introspection;
#[cfg(not(feature = "test-jwks"))]
mod lifecycle;
#[cfg(not(feature = "test-jwks"))]
mod metrics;
#[cfg(not(feature = "test-jwks"))]
mod mock_subgraphs;
#[cfg(not(feature = "test-jwks"))]
mod operation_limits;
#[cfg(not(feature = "test-jwks"))]
mod operation_name;
#[cfg(not(feature = "test-jwks"))]
mod query_planner;
#[cfg(not(feature = "test-jwks"))]
mod subgraph_response;
#[cfg(not(feature = "test-jwks"))]
mod supergraph;
#[cfg(not(feature = "test-jwks"))]
mod traffic_shaping;
#[cfg(not(feature = "test-jwks"))]
mod typename;

// In the CI environment we only install PostgreSQL on x86_64 Linux
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
mod postgres;
// In the CI environment we only install PostgreSQL on x86_64 Linux
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
mod response_cache;
// In the CI environment we only install Redis on x86_64 Linux
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
mod redis;
#[cfg(not(feature = "test-jwks"))]
mod rhai;
#[cfg(not(feature = "test-jwks"))]
mod subscription_load_test;
#[cfg(not(feature = "test-jwks"))]
mod subscriptions;
#[cfg(not(feature = "test-jwks"))]
mod telemetry;
#[cfg(not(feature = "test-jwks"))]
mod validation;

use jsonpath_lib::Selector;
use serde_json::Value;
use tower::BoxError;

#[allow(dead_code)]
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
