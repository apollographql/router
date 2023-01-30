//!  (A)utomatic (P)ersisted (Q)ueries cache.
//!
//!  For more information on APQ see:
//!  <https://www.apollographql.com/docs/apollo-server/performance/apq/>

// This entire file is license key functionality

use http::header::CACHE_CONTROL;
use http::HeaderValue;
use serde::Deserialize;
use serde_json_bytes::json;
use serde_json_bytes::Value;
use sha2::Digest;
use sha2::Sha256;

use crate::cache::DeduplicatingCache;
use crate::services::RouterResponse;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;

const DONT_CACHE_RESPONSE_VALUE: &str = "private, no-cache, must-revalidate";

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

    pub(crate) async fn supergraph_request(
        &self,
        request: SupergraphRequest,
    ) -> Result<SupergraphRequest, SupergraphResponse> {
        apq_request(&self.cache, request).await
    }

    pub(crate) fn router_response(&self, response: RouterResponse) -> RouterResponse {
        set_cache_control_headers(response)
    }
}

/// Persisted query errors (especially "not found") need to be uncached, because
/// hopefully we're about to fill in the APQ cache and the same request will
/// succeed next time.
fn set_cache_control_headers(mut response: RouterResponse) -> RouterResponse {
    if let Ok(Some(true)) = &response.context.get::<_, bool>("persisted_query_miss") {
        response.response.headers_mut().insert(
            CACHE_CONTROL,
            HeaderValue::from_static(DONT_CACHE_RESPONSE_VALUE),
        );
    }
    response
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
                let _ = request.context.insert("persisted_query_miss", true);
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

pub(crate) fn calculate_hash_for_query(query: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(query);
    hex::encode(hasher.finalize())
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
    use crate::services::layers::content_negociation::ACCEPTS_JSON_CONTEXT_KEY;
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
                .context(req.context)
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
            .context(new_context())
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();

        let apq_response = router_service.call(hash_only).await.unwrap();

        // make sure clients won't cache apq missed response
        assert_eq!(
            DONT_CACHE_RESPONSE_VALUE,
            apq_response.response.headers().get(CACHE_CONTROL).unwrap()
        );

        let apq_error = apq_response
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
            .context(new_context())
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();
        let full_response = router_service.call(with_query).await.unwrap();

        // the cache control header shouldn't have been tampered with
        assert!(full_response
            .response
            .headers()
            .get(CACHE_CONTROL)
            .is_none());

        let second_hash_only = SupergraphRequest::fake_builder()
            .extension("persistedQuery", persisted.clone())
            .context(new_context())
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();
        let apq_response = router_service.call(second_hash_only).await.unwrap();

        // the cache control header shouldn't have been tampered with
        assert!(apq_response.response.headers().get(CACHE_CONTROL).is_none());
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
                .context(req.context)
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
            .context(new_context())
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();

        let request_builder =
            SupergraphRequest::fake_builder().extension("persistedQuery", persisted.clone());

        let second_hash_only = request_builder
            .context(new_context())
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();

        let request_builder =
            SupergraphRequest::fake_builder().extension("persistedQuery", persisted.clone());

        let with_query = request_builder
            .query("{__typename}".to_string())
            .context(new_context())
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

    fn new_context() -> Context {
        let context = Context::new();
        context.insert(ACCEPTS_JSON_CONTEXT_KEY, true).unwrap();
        context
    }
}
