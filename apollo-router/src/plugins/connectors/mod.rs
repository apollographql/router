pub(crate) mod configuration;
pub(crate) mod handle_responses;
pub(crate) mod incompatible;
pub(crate) mod make_requests;
pub(crate) mod plugin;
pub(crate) mod query_plans;
pub(crate) mod request_limit;
pub(crate) mod tracing;

#[cfg(test)]
pub(crate) mod tests;

use apollo_federation::connectors::runtime::inputs::ContextReader;

impl ContextReader for &crate::Context {
    fn get_key(&self, key: &str) -> Option<serde_json_bytes::Value> {
        match self.get::<&str, serde_json_bytes::Value>(key) {
            Ok(Some(value)) => Some(value.clone()),
            _ => None,
        }
    }
}
