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
    fn to_json_map(
        &self,
    ) -> serde_json_bytes::Map<serde_json_bytes::ByteString, serde_json_bytes::Value> {
        self.iter()
            .map(|entry| (entry.key().as_str().into(), entry.value().clone()))
            .collect()
    }
}
