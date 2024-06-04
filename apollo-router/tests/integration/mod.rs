mod batching;
#[path = "../common.rs"]
pub(crate) mod common;
pub(crate) use common::IntegrationTest;

mod coprocessor;
mod docs;
mod file_upload;
mod lifecycle;
mod operation_limits;

#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
mod redis;
mod rhai;
mod subscription;
mod telemetry;
mod validation;
