// tonic does not derive `Eq` for the gRPC message types, which causes a warning from Clippy. The
// current suggestion is to explicitly allow the lint in the module that imports the protos.
// Read more: https://github.com/hyperium/tonic/issues/1056
#![allow(clippy::derive_partial_eq_without_eq)]

use graphql_client::GraphQLQuery;

use crate::uplink::schema_stream::supergraph_sdl_query::FetchErrorCode;
use crate::uplink::schema_stream::supergraph_sdl_query::SupergraphSdlQueryRouterConfig;
use crate::uplink::UplinkRequest;
use crate::uplink::UplinkResponse;

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/uplink/schema_query.graphql",
    schema_path = "src/uplink/uplink.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize",
    deprecated = "warn"
)]

pub(crate) struct SupergraphSdlQuery;

impl From<UplinkRequest> for supergraph_sdl_query::Variables {
    fn from(req: UplinkRequest) -> Self {
        supergraph_sdl_query::Variables {
            api_key: req.api_key,
            graph_ref: req.graph_ref,
            if_after_id: req.id,
        }
    }
}

impl From<supergraph_sdl_query::ResponseData> for UplinkResponse<String> {
    fn from(response: supergraph_sdl_query::ResponseData) -> Self {
        match response.router_config {
            SupergraphSdlQueryRouterConfig::RouterConfigResult(result) => UplinkResponse::New {
                response: result.supergraph_sdl,
                id: result.id,
                // this will truncate the number of seconds to under u64::MAX, which should be
                // a large enough delay anyway
                delay: result.min_delay_seconds as u64,
            },
            SupergraphSdlQueryRouterConfig::Unchanged(response) => UplinkResponse::Unchanged {
                id: Some(response.id),
                delay: Some(response.min_delay_seconds as u64),
            },
            SupergraphSdlQueryRouterConfig::FetchError(err) => UplinkResponse::Error {
                retry_later: err.code == FetchErrorCode::RETRY_LATER,
                code: match err.code {
                    FetchErrorCode::AUTHENTICATION_FAILED => "AUTHENTICATION_FAILED".to_string(),
                    FetchErrorCode::ACCESS_DENIED => "ACCESS_DENIED".to_string(),
                    FetchErrorCode::UNKNOWN_REF => "UNKNOWN_REF".to_string(),
                    FetchErrorCode::RETRY_LATER => "RETRY_LATER".to_string(),
                    FetchErrorCode::NOT_IMPLEMENTED_ON_THIS_INSTANCE => {
                        "NOT_IMPLEMENTED_ON_THIS_INSTANCE".to_string()
                    }
                    FetchErrorCode::Other(other) => other,
                },
                message: err.message,
            },
        }
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;
    use std::time::Duration;

    use futures::stream::StreamExt;
    use url::Url;

    use crate::uplink::schema_stream::SupergraphSdlQuery;
    use crate::uplink::stream_from_uplink;
    use crate::uplink::Endpoints;
    use crate::uplink::UplinkConfig;
    use crate::uplink::AWS_URL;
    use crate::uplink::GCP_URL;

    #[tokio::test]
    async fn integration_test() {
        for url in &[GCP_URL, AWS_URL] {
            if let (Ok(apollo_key), Ok(apollo_graph_ref)) = (
                std::env::var("TEST_APOLLO_KEY"),
                std::env::var("TEST_APOLLO_GRAPH_REF"),
            ) {
                let results = stream_from_uplink::<SupergraphSdlQuery, String>(UplinkConfig {
                    apollo_key,
                    apollo_graph_ref,
                    endpoints: Some(Endpoints::fallback(vec![
                        Url::from_str(url).expect("url must be valid")
                    ])),
                    poll_interval: Duration::from_secs(1),
                    timeout: Duration::from_secs(5),
                })
                .take(1)
                .collect::<Vec<_>>()
                .await;

                let schema = results
                    .get(0)
                    .unwrap_or_else(|| panic!("expected one result from {}", url))
                    .as_ref()
                    .unwrap_or_else(|_| panic!("schema should be OK from {}", url));
                assert!(schema.contains("type Product"))
            }
        }
    }
}
