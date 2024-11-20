mod batching;
#[path = "../common.rs"]
pub(crate) mod common;
pub(crate) use common::IntegrationTest;

mod coprocessor;
mod docs;
mod file_upload;
mod introspection;
mod lifecycle;
mod operation_limits;
mod operation_name;
mod query_planner;
mod subgraph_response;
mod supergraph;
mod traffic_shaping;
mod typename;

#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
mod redis;
mod rhai;
mod subscription;
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
