// tonic does not derive `Eq` for the gRPC message types, which causes a warning from Clippy. The
// current suggestion is to explicitly allow the lint in the module that imports the protos.
// Read more: https://github.com/hyperium/tonic/issues/1056
#![allow(clippy::derive_partial_eq_without_eq)]

use graphql_client::GraphQLQuery;

use crate::uplink::persisted_queries_manifest_stream::persisted_queries_manifest_query::FetchErrorCode;
use crate::uplink::persisted_queries_manifest_stream::persisted_queries_manifest_query::PersistedQueriesManifestQueryPersistedQueries;
use crate::uplink::persisted_queries_manifest_stream::persisted_queries_manifest_query::PersistedQueriesManifestQueryPersistedQueriesOnPersistedQueriesResultChunks;
use crate::uplink::UplinkRequest;
use crate::uplink::UplinkResponse;

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/uplink/persisted_queries_manifest_query.graphql",
    schema_path = "src/uplink/uplink.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize",
    deprecated = "warn"
)]

pub(crate) struct PersistedQueriesManifestQuery;

impl From<UplinkRequest> for persisted_queries_manifest_query::Variables {
    fn from(req: UplinkRequest) -> Self {
        persisted_queries_manifest_query::Variables {
            api_key: req.api_key,
            graph_ref: req.graph_ref,
            if_after_id: req.id,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct PersistedQueriesManifestChunk {
    pub(crate) id: String,
    pub(crate) urls: Vec<String>,
}

impl PersistedQueriesManifestChunk {
    fn from_query_chunks(
        query_chunks: &PersistedQueriesManifestQueryPersistedQueriesOnPersistedQueriesResultChunks,
    ) -> Self {
        Self {
            id: query_chunks.id.clone(),
            urls: query_chunks.urls.clone(),
        }
    }
}

pub(crate) type PersistedQueriesManifestChunks = Vec<PersistedQueriesManifestChunk>;
pub(crate) type MaybePersistedQueriesManifestChunks = Option<PersistedQueriesManifestChunks>;

impl From<persisted_queries_manifest_query::ResponseData>
    for UplinkResponse<MaybePersistedQueriesManifestChunks>
{
    fn from(response: persisted_queries_manifest_query::ResponseData) -> Self {
        match response.persisted_queries {
            PersistedQueriesManifestQueryPersistedQueries::PersistedQueriesResult(response) => {
                if let Some(chunks) = response.chunks {
                    let chunks = chunks
                        .iter()
                        .map(PersistedQueriesManifestChunk::from_query_chunks)
                        .collect();
                    UplinkResponse::New {
                        response: Some(chunks),
                        id: response.id,
                        // this will truncate the number of seconds to under u64::MAX, which should be
                        // a large enough delay anyway
                        delay: response.min_delay_seconds as u64,
                    }
                } else {
                    UplinkResponse::New {
                        // no persisted query list is associated with this variant
                        response: None,
                        id: response.id,
                        delay: response.min_delay_seconds as u64,
                    }
                }
            }
            PersistedQueriesManifestQueryPersistedQueries::Unchanged(response) => {
                UplinkResponse::Unchanged {
                    id: Some(response.id),
                    delay: Some(response.min_delay_seconds as u64),
                }
            }
            PersistedQueriesManifestQueryPersistedQueries::FetchError(err) => {
                UplinkResponse::Error {
                    retry_later: err.code == FetchErrorCode::RETRY_LATER,
                    code: match err.code {
                        FetchErrorCode::AUTHENTICATION_FAILED => {
                            "AUTHENTICATION_FAILED".to_string()
                        }
                        FetchErrorCode::ACCESS_DENIED => "ACCESS_DENIED".to_string(),
                        FetchErrorCode::UNKNOWN_REF => "UNKNOWN_REF".to_string(),
                        FetchErrorCode::RETRY_LATER => "RETRY_LATER".to_string(),
                        FetchErrorCode::NOT_IMPLEMENTED_ON_THIS_INSTANCE => {
                            "NOT_IMPLEMENTED_ON_THIS_INSTANCE".to_string()
                        }
                        FetchErrorCode::Other(other) => other,
                    },
                    message: err.message,
                }
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;
    use std::time::Duration;

    use futures::stream::StreamExt;
    use url::Url;

    use crate::uplink::persisted_queries_manifest_stream::MaybePersistedQueriesManifestChunks;
    use crate::uplink::persisted_queries_manifest_stream::PersistedQueriesManifestQuery;
    use crate::uplink::stream_from_uplink;
    use crate::uplink::Endpoints;
    use crate::uplink::UplinkConfig;
    use crate::uplink::GCP_URL;

    #[tokio::test]
    async fn integration_test() {
        if let (Ok(apollo_key), Ok(apollo_graph_ref)) = (
            std::env::var("TEST_APOLLO_KEY"),
            std::env::var("TEST_APOLLO_GRAPH_REF"),
        ) {
            // TODO: Add AWS_URL when that exists
            for url in &[GCP_URL] {
                let results = stream_from_uplink::<
                    PersistedQueriesManifestQuery,
                    MaybePersistedQueriesManifestChunks,
                >(UplinkConfig {
                    apollo_key: apollo_key.clone(),
                    apollo_graph_ref: apollo_graph_ref.clone(),
                    endpoints: Some(Endpoints::fallback(vec![
                        Url::from_str(url).expect("url must be valid")
                    ])),
                    poll_interval: Duration::from_secs(1),
                    timeout: Duration::from_secs(5),
                })
                .take(1)
                .collect::<Vec<_>>()
                .await;

                let persisted_query_manifest = results
                    .first()
                    .unwrap_or_else(|| panic!("expected one result from {}", url))
                    .as_ref()
                    .unwrap_or_else(|_| panic!("schema should be OK from {}", url))
                    .as_ref()
                    .unwrap();
                assert!(!persisted_query_manifest.is_empty())
            }
        }
    }
}
