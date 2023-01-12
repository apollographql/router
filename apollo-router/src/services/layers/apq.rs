//!  (A)utomatic (P)ersisted (Q)ueries cache.
//!
//!  For more information on APQ see:
//!  <https://www.apollographql.com/docs/apollo-server/performance/apq/>

// This entire file is license key functionality

use serde::Deserialize;
use serde_json_bytes::json;
use serde_json_bytes::Value;
use sha2::Digest;
use sha2::Sha256;

use crate::cache::DeduplicatingCache;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;

/// A persisted query.
#[derive(Deserialize, Clone, Debug)]
struct PersistedQuery {
    #[allow(unused)]
    version: u8,
    #[serde(rename = "sha256Hash")]
    sha256hash: String,
}

/// [`Layer`] for APQ implementation.
#[derive(Clone)]
pub(crate) struct APQLayer {
    cache: DeduplicatingCache<String, String>,
}

impl APQLayer {
    pub(crate) fn with_cache(cache: DeduplicatingCache<String, String>) -> Self {
        Self { cache }
    }

    pub(crate) async fn request(
        &self,
        request: SupergraphRequest,
    ) -> Result<SupergraphRequest, SupergraphResponse> {
        apq_request(&self.cache, request).await
    }
}

async fn apq_request(
    cache: &DeduplicatingCache<String, String>,
    mut request: SupergraphRequest,
) -> Result<SupergraphRequest, SupergraphResponse> {
    let maybe_query_hash: Option<(String, Vec<u8>)> = request
        .supergraph_request
        .body()
        .extensions
        .get("persistedQuery")
        .and_then(|value| serde_json_bytes::from_value::<PersistedQuery>(value.clone()).ok())
        .and_then(|persisted_query| {
            hex::decode(persisted_query.sha256hash.as_bytes())
                .ok()
                .map(|decoded| (persisted_query.sha256hash, decoded))
        });

    let body_query = request.supergraph_request.body().query.clone();

    match (maybe_query_hash, body_query) {
        (Some((query_hash, query_hash_bytes)), Some(query)) => {
            if query_matches_hash(query.as_str(), query_hash_bytes.as_slice()) {
                tracing::trace!("apq: cache insert");
                let _ = request.context.insert("persisted_query_hit", false);
                cache.insert(redis_key(&query_hash), query).await;
            } else {
                tracing::warn!("apq: graphql request doesn't match provided sha256Hash");
            }
            Ok(request)
        }
        (Some((apq_hash, _)), _) => {
            if let Ok(cached_query) = cache.get(&redis_key(&apq_hash)).await.get().await {
                let _ = request.context.insert("persisted_query_hit", true);
                tracing::trace!("apq: cache hit");
                request.supergraph_request.body_mut().query = Some(cached_query);
                Ok(request)
            } else {
                tracing::trace!("apq: cache miss");
                let errors = vec![crate::error::Error {
                    message: "PersistedQueryNotFound".to_string(),
                    locations: Default::default(),
                    path: Default::default(),
                    extensions: serde_json_bytes::from_value(json!({
                          "code": "PERSISTED_QUERY_NOT_FOUND",
                          "exception": {
                          "stacktrace": [
                              "PersistedQueryNotFoundError: PersistedQueryNotFound",
                          ],
                      },
                    }))
                    .unwrap(),
                }];
                let res = SupergraphResponse::builder()
                    .data(Value::default())
                    .errors(errors)
                    .context(request.context)
                    .build()
                    .expect("response is valid");

                Err(res)
            }
        }
        _ => Ok(request),
    }
}

fn query_matches_hash(query: &str, hash: &[u8]) -> bool {
    let mut digest = Sha256::new();
    digest.update(query.as_bytes());
    hash == digest.finalize().as_slice()
}

fn redis_key(query_hash: &str) -> String {
    format!("apq\0{query_hash}")
}

#[cfg(test)]
mod apq_tests {
    use std::borrow::Cow;

    use futures::StreamExt;
    use serde_json_bytes::json;
    use tower::Service;

    use super::*;
    use crate::error::Error;
    use crate::graphql::Response;
    use crate::services::router_service::from_supergraph_mock_callback;
    use crate::Context;

    #[tokio::test]
    async fn it_works() {
        let hash = Cow::from("ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b38");
        let hash2 = hash.clone();

        let expected_apq_miss_error = Error {
            message: "PersistedQueryNotFound".to_string(),
            locations: Default::default(),
            path: Default::default(),
            extensions: serde_json_bytes::from_value(json!({
                  "code": "PERSISTED_QUERY_NOT_FOUND",
                  "exception": {
                  "stacktrace": [
                      "PersistedQueryNotFoundError: PersistedQueryNotFound",
                  ],
              },
            }))
            .unwrap(),
        };

        let mut router_service = from_supergraph_mock_callback(move |req| {
            let body = req.supergraph_request.body();
            let as_json = body.extensions.get("persistedQuery").unwrap();

            let persisted_query: PersistedQuery =
                serde_json_bytes::from_value(as_json.clone()).unwrap();

            assert_eq!(persisted_query.sha256hash, hash2);

            assert!(body.query.is_some());

            let hash = hex::decode(hash2.as_bytes()).unwrap();

            assert!(query_matches_hash(
                body.query.clone().unwrap().as_str(),
                hash.as_slice()
            ));

            Ok(SupergraphResponse::fake_builder()
                .build()
                .expect("expecting valid request"))
        })
        .await;

        let persisted = json!({
            "version" : 1,
            "sha256Hash" : "ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b38"
        });

        let hash_only = SupergraphRequest::fake_builder()
            .extension("persistedQuery", persisted.clone())
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();

        let apq_error = router_service
            .call(hash_only)
            .await
            .unwrap()
            .into_graphql_response_stream()
            .await
            .next()
            .await
            .unwrap()
            .unwrap();

        assert_error_matches(&expected_apq_miss_error, apq_error);

        let with_query = SupergraphRequest::fake_builder()
            .extension("persistedQuery", persisted.clone())
            .query("{__typename}".to_string())
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();
        router_service.call(with_query).await.unwrap();

        let second_hash_only = SupergraphRequest::fake_builder()
            .extension("persistedQuery", persisted.clone())
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();
        router_service.call(second_hash_only).await.unwrap();
    }

    #[tokio::test]
    async fn it_doesnt_update_the_cache_if_the_hash_is_not_valid() {
        let hash = Cow::from("ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b36");
        let hash2 = hash.clone();

        let expected_apq_miss_error = Error {
            message: "PersistedQueryNotFound".to_string(),
            locations: Default::default(),
            path: Default::default(),
            extensions: serde_json_bytes::from_value(json!({
                  "code": "PERSISTED_QUERY_NOT_FOUND",
                  "exception": {
                  "stacktrace": [
                      "PersistedQueryNotFoundError: PersistedQueryNotFound",
                  ],
              },
            }))
            .unwrap(),
        };

        let mut router_service = from_supergraph_mock_callback(move |req| {
            let body = req.supergraph_request.body();
            let as_json = body.extensions.get("persistedQuery").unwrap();

            let persisted_query: PersistedQuery =
                serde_json_bytes::from_value(as_json.clone()).unwrap();

            assert_eq!(persisted_query.sha256hash, hash2);

            assert!(body.query.is_some());

            Ok(SupergraphResponse::fake_builder()
                .build()
                .expect("expecting valid request"))
        })
        .await;

        let persisted = json!({
            "version" : 1,
            "sha256Hash" : "ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b36"
        });

        let request_builder =
            SupergraphRequest::fake_builder().extension("persistedQuery", persisted.clone());

        let hash_only = request_builder
            .context(Context::new())
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();

        let request_builder =
            SupergraphRequest::fake_builder().extension("persistedQuery", persisted.clone());

        let second_hash_only = request_builder
            .context(Context::new())
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();

        let request_builder =
            SupergraphRequest::fake_builder().extension("persistedQuery", persisted.clone());

        let with_query = request_builder
            .query("{__typename}".to_string())
            .context(Context::new())
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();

        // This apq call will miss the APQ cache
        let apq_error = router_service
            .call(hash_only)
            .await
            .unwrap()
            .into_graphql_response_stream()
            .await
            .next()
            .await
            .unwrap()
            .unwrap();

        assert_error_matches(&expected_apq_miss_error, apq_error);

        // sha256 is wrong, apq insert won't happen
        router_service.call(with_query).await.unwrap();

        // apq insert failed, this call will miss
        let second_apq_error = router_service
            .call(second_hash_only)
            .await
            .unwrap()
            .into_graphql_response_stream()
            .await
            .next()
            .await
            .unwrap()
            .unwrap();

        assert_error_matches(&expected_apq_miss_error, second_apq_error);
    }

    fn assert_error_matches(expected_error: &Error, res: Response) {
        assert_eq!(&res.errors[0], expected_error);
    }
}
