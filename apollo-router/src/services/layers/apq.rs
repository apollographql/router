//!  (A)utomatic (P)ersisted (Q)ueries cache.
//!
//!  For more information on APQ see:
//!  <https://www.apollographql.com/docs/apollo-server/performance/apq/>

use http::header::CACHE_CONTROL;
use http::HeaderValue;
use http::StatusCode;
use serde::Deserialize;
use serde_json_bytes::json;
use serde_json_bytes::Value;
use sha2::Digest;
use sha2::Sha256;

use crate::cache::DeduplicatingCache;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;

const DONT_CACHE_RESPONSE_VALUE: &str = "private, no-cache, must-revalidate";
static DONT_CACHE_HEADER_VALUE: HeaderValue = HeaderValue::from_static(DONT_CACHE_RESPONSE_VALUE);

/// A persisted query.
#[derive(Deserialize, Clone, Debug)]
pub(crate) struct PersistedQuery {
    #[allow(unused)]
    pub(crate) version: u8,
    #[serde(rename = "sha256Hash")]
    pub(crate) sha256hash: String,
}

impl PersistedQuery {
    /// Attempt to extract a `PersistedQuery` from a `&SupergraphRequest`
    pub(crate) fn maybe_from_request(request: &SupergraphRequest) -> Option<Self> {
        request
            .supergraph_request
            .body()
            .extensions
            .get("persistedQuery")
            .and_then(|value| serde_json_bytes::from_value(value.clone()).ok())
    }

    /// Attempt to decode the sha256 hash in a [`PersistedQuery`]
    pub(crate) fn decode_hash(self) -> Option<(String, Vec<u8>)> {
        hex::decode(self.sha256hash.as_bytes())
            .ok()
            .map(|decoded| (self.sha256hash, decoded))
    }
}

/// [`Layer`] for APQ implementation.
#[derive(Clone)]
pub(crate) struct APQLayer {
    /// set to None if APQ is disabled
    cache: Option<DeduplicatingCache<String, String>>,
}

impl APQLayer {
    pub(crate) fn with_cache(cache: DeduplicatingCache<String, String>) -> Self {
        Self { cache: Some(cache) }
    }

    pub(crate) fn disabled() -> Self {
        Self { cache: None }
    }

    pub(crate) async fn supergraph_request(
        &self,
        request: SupergraphRequest,
    ) -> Result<SupergraphRequest, SupergraphResponse> {
        match self.cache.as_ref() {
            Some(cache) => apq_request(cache, request).await,
            None => disabled_apq_request(request).await,
        }
    }
}

async fn apq_request(
    cache: &DeduplicatingCache<String, String>,
    mut request: SupergraphRequest,
) -> Result<SupergraphRequest, SupergraphResponse> {
    let maybe_query_hash =
        PersistedQuery::maybe_from_request(&request).and_then(PersistedQuery::decode_hash);

    let body_query = request.supergraph_request.body().query.clone();

    match (maybe_query_hash, body_query) {
        (Some((query_hash, query_hash_bytes)), Some(query)) => {
            if query_matches_hash(query.as_str(), query_hash_bytes.as_slice()) {
                tracing::trace!("apq: cache insert");
                let _ = request.context.insert("persisted_query_register", true);
                cache.insert(redis_key(&query_hash), query).await;
                Ok(request)
            } else {
                tracing::debug!("apq: graphql request doesn't match provided sha256Hash");
                let errors = vec![crate::error::Error {
                    message: "provided sha does not match query".to_string(),
                    locations: Default::default(),
                    path: Default::default(),
                    extensions: serde_json_bytes::from_value(json!({
                      "code": "PERSISTED_QUERY_HASH_MISMATCH",
                    }))
                    .unwrap(),
                }];
                let res = SupergraphResponse::builder()
                    .status_code(StatusCode::BAD_REQUEST)
                    .data(Value::default())
                    .errors(errors)
                    .context(request.context)
                    .build()
                    .expect("response is valid");
                Err(res)
            }
        }
        (Some((apq_hash, _)), _) => {
            if let Ok(cached_query) = cache.get(&redis_key(&apq_hash)).await.get().await {
                let _ = request.context.insert("persisted_query_hit", true);
                tracing::trace!("apq: cache hit");
                request.supergraph_request.body_mut().query = Some(cached_query);
                Ok(request)
            } else {
                let _ = request.context.insert("persisted_query_hit", false);
                tracing::trace!("apq: cache miss");
                let errors = vec![crate::error::Error {
                    message: "PersistedQueryNotFound".to_string(),
                    locations: Default::default(),
                    path: Default::default(),
                    extensions: serde_json_bytes::from_value(json!({
                      "code": "PERSISTED_QUERY_NOT_FOUND",
                    }))
                    .unwrap(),
                }];
                let res = SupergraphResponse::builder()
                    .data(Value::default())
                    .errors(errors)
                    // Persisted query errors (especially "not found") need to be uncached, because
                    // hopefully we're about to fill in the APQ cache and the same request will
                    // succeed next time.
                    .header(CACHE_CONTROL, DONT_CACHE_HEADER_VALUE.clone())
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

async fn disabled_apq_request(
    request: SupergraphRequest,
) -> Result<SupergraphRequest, SupergraphResponse> {
    if request
        .supergraph_request
        .body()
        .extensions
        .contains_key("persistedQuery")
    {
        let errors = vec![crate::error::Error {
            message: "PersistedQueryNotSupported".to_string(),
            locations: Default::default(),
            path: Default::default(),
            extensions: serde_json_bytes::from_value(json!({
              "code": "PERSISTED_QUERY_NOT_SUPPORTED",
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
    } else {
        Ok(request)
    }
}
#[cfg(test)]
mod apq_tests {
    use std::borrow::Cow;
    use std::sync::Arc;

    use futures::StreamExt;
    use http::StatusCode;
    use serde_json_bytes::json;
    use tower::Service;

    use super::*;
    use crate::error::Error;
    use crate::graphql::Response;
    use crate::services::router::ClientRequestAccepts;
    use crate::services::router_service::from_supergraph_mock_callback;
    use crate::services::router_service::from_supergraph_mock_callback_and_configuration;
    use crate::Configuration;
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

        // We need to yield here to make sure the router
        // runs the Drop implementation of the deduplicating cache Entry.
        tokio::task::yield_now().await;

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
        let insert_failed_response = router_service.call(with_query).await.unwrap();

        assert_eq!(
            StatusCode::BAD_REQUEST,
            insert_failed_response.response.status()
        );

        let graphql_response = insert_failed_response
            .into_graphql_response_stream()
            .await
            .next()
            .await
            .unwrap()
            .unwrap();
        let expected_apq_insert_failed_error = Error {
            message: "provided sha does not match query".to_string(),
            locations: Default::default(),
            path: Default::default(),
            extensions: serde_json_bytes::from_value(json!({
              "code": "PERSISTED_QUERY_HASH_MISMATCH",
            }))
            .unwrap(),
        };
        assert_eq!(graphql_response.errors[0], expected_apq_insert_failed_error);

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

    #[tokio::test]
    async fn return_not_supported_when_disabled() {
        let expected_apq_miss_error = Error {
            message: "PersistedQueryNotSupported".to_string(),
            locations: Default::default(),
            path: Default::default(),
            extensions: serde_json_bytes::from_value(json!({
              "code": "PERSISTED_QUERY_NOT_SUPPORTED",
            }))
            .unwrap(),
        };

        let mut config = Configuration::default();
        config.apq.enabled = false;

        let mut router_service = from_supergraph_mock_callback_and_configuration(
            move |req| {
                Ok(SupergraphResponse::fake_builder()
                    .context(req.context)
                    .build()
                    .expect("expecting valid request"))
            },
            Arc::new(config),
        )
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

        let with_query_response = router_service.call(with_query).await.unwrap();

        let apq_error = with_query_response
            .into_graphql_response_stream()
            .await
            .next()
            .await
            .unwrap()
            .unwrap();

        assert_error_matches(&expected_apq_miss_error, apq_error);

        let without_apq = SupergraphRequest::fake_builder()
            .query("{__typename}".to_string())
            .context(new_context())
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();

        let without_apq_response = router_service.call(without_apq).await.unwrap();

        let without_apq_graphql_response = without_apq_response
            .into_graphql_response_stream()
            .await
            .next()
            .await
            .unwrap()
            .unwrap();

        assert!(without_apq_graphql_response.errors.is_empty());
    }

    fn assert_error_matches(expected_error: &Error, res: Response) {
        assert_eq!(&res.errors[0], expected_error);
    }

    fn new_context() -> Context {
        let context = Context::new();
        context.private_entries.lock().insert(ClientRequestAccepts {
            json: true,
            ..Default::default()
        });

        context
    }
}
