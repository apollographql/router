//! Persisted Query ID extractor

use crate::services::layers::apq::PersistedQuery;
use crate::services::SupergraphRequest;

#[derive(Debug, Clone)]
pub(crate) struct PersistedQueryIdExtractor;

impl PersistedQueryIdExtractor {
    pub(crate) fn extract_id(request: &SupergraphRequest) -> Option<String> {
        PersistedQuery::maybe_from_request(request).map(|pq| pq.sha256hash)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn build_supergraph_request_with_pq_extension(
        persisted: &serde_json::Value,
    ) -> SupergraphRequest {
        SupergraphRequest::fake_builder()
            .extension("persistedQuery", persisted.clone())
            .build()
            .unwrap()
    }

    fn assert_can_extract_id(expected_id: String, request: SupergraphRequest) {
        assert_eq!(
            PersistedQueryIdExtractor::extract_id(&request),
            Some(expected_id)
        )
    }

    fn assert_cannot_extract_id(request: SupergraphRequest) {
        assert_eq!(PersistedQueryIdExtractor::extract_id(&request), None)
    }

    #[test]
    fn it_cannot_extract_id_from_request_extensions_without_version() {
        let hash = "ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b36".to_string();
        let persisted = json!({ "sha256Hash": &hash });
        assert_cannot_extract_id(build_supergraph_request_with_pq_extension(&persisted))
    }

    #[test]
    fn it_can_extract_id_from_request_extensions_with_version() {
        let hash = "ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b36".to_string();
        let persisted = json!({ "sha256Hash": &hash, "version": 1 });
        assert_can_extract_id(hash, build_supergraph_request_with_pq_extension(&persisted))
    }
}
