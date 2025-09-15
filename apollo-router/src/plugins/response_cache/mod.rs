pub(crate) mod cache_control;
pub(crate) mod cache_key;
pub(crate) mod invalidation;
pub(crate) mod invalidation_endpoint;
pub(crate) mod metrics;
pub(crate) mod plugin;
pub(crate) mod postgres;
pub(crate) mod serde_blake3;
#[cfg(all(
    test,
    any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
))]
pub(crate) mod tests;

pub(super) trait ErrorCode {
    fn code(&self) -> &'static str;
}
