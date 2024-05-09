mod batching;
#[path = "../common.rs"]
pub(crate) mod common;
pub(crate) use common::IntegrationTest;

mod docs;
mod file_upload;
mod lifecycle;
mod operation_limits;

#[cfg(any(not(feature = "ci"), target_arch = "x86_64"))]
mod redis;
mod rhai;
mod subscription;
mod telemetry;
mod validation;
