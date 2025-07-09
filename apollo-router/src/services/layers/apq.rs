//! (A)utomatic (P)ersisted (Q)ueries cache.
//!
//! For more information on APQ see:
//! <https://www.apollographql.com/docs/apollo-server/performance/apq/>

use http::HeaderValue;
use http::StatusCode;
use http::header::CACHE_CONTROL;
use serde::Deserialize;
use serde_json_bytes::Value;
use sha2::Digest;
use sha2::Sha256;

use crate::cache::DeduplicatingCache;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;

const DONT_CACHE_RESPONSE_VALUE: &str = "private, no-cache, must-revalidate";
static DONT_CACHE_HEADER_VALUE: HeaderValue = HeaderValue::from_static(DONT_CACHE_RESPONSE_VALUE);
pub(crate) const PERSISTED_QUERY_CACHE_HIT: &str = "apollo::apq::cache_hit";
pub(crate) const PERSISTED_QUERY_REGISTERED: &str = "apollo::apq::registered";

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

/// A layer-like type implementing Automatic Persisted Queries.
#[derive(Clone)]
pub(crate) struct APQLayer {
    /// set to None if APQ is disabled
    cache: Option<DeduplicatingCache<String, String>>,
}

impl APQLayer {
    pub(crate) fn activate(&self) {
        if let Some(cache) = &self.cache {
            cache.activate();
        }
    }
}

impl APQLayer {
    pub(crate) fn with_cache(cache: DeduplicatingCache<String, String>) -> Self {
        Self { cache: Some(cache) }
    }

    pub(crate) fn disabled() -> Self {
        Self { cache: None }
    }

    /// Supergraph service implementation for Automatic Persisted Queries.
    ///
    /// For more information about APQ:
    /// https://www.apollographql.com/docs/apollo-server/performance/apq.
    ///
    /// If APQ is disabled, it rejects requests that try to use a persisted query hash.
    /// If APQ is enabled, requests using APQ will populate the cache and use the cache as needed,
    /// see [`apq_request`] for details.
    ///
    /// This must happen before GraphQL query parsing.
    ///
    /// This functions similarly to a checkpoint service, short-circuiting the pipeline on error
    /// (using an `Err()` return value).
    /// The user of this function is responsible for propagating short-circuiting.
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

/// Used when APQ is enabled.
///
/// If the request contains a hash and a query string, that query is added to the APQ cache.
/// Then, the client can submit only the hash and not the query string on subsequent requests.
/// The request is rejected if the hash does not match the query string.
///
/// If the request contains only a hash, attempts to read the query from the APQ cache, and
/// populates the query string in the request body.
/// The request is rejected if the hash is not present in the cache.
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
                let _ = request.context.insert(PERSISTED_QUERY_REGISTERED, true);
                let query = query.to_owned();
                let cache = cache.clone();
                tokio::spawn(async move {
                    cache.insert(redis_key(&query_hash), query).await;
                });
                Ok(request)
            } else {
                tracing::debug!("apq: graphql request doesn't match provided sha256Hash");
                let errors = vec![
                    crate::error::Error::builder()
                        .message("provided sha does not match query".to_string())
                        .locations(Default::default())
                        .extension_code("PERSISTED_QUERY_HASH_MISMATCH")
                        .build(),
                ];
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
            if let Ok(cached_query) = cache
                .get(&redis_key(&apq_hash), |_| Ok(()))
                .await
                .get()
                .await
            {
                let _ = request.context.insert(PERSISTED_QUERY_CACHE_HIT, true);
                tracing::trace!("apq: cache hit");
                request.supergraph_request.body_mut().query = Some(cached_query);
                Ok(request)
            } else {
                let _ = request.context.insert(PERSISTED_QUERY_CACHE_HIT, false);
                tracing::trace!("apq: cache miss");
                let errors = vec![
                    crate::error::Error::builder()
                        .message("PersistedQueryNotFound".to_string())
                        .locations(Default::default())
                        .extension_code("PERSISTED_QUERY_NOT_FOUND")
                        .build(),
                ];
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
    format!("apq:{query_hash}")
}

pub(crate) fn calculate_hash_for_query(query: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(query);
    hex::encode(hasher.finalize())
}

/// Used when APQ is disabled. Rejects requests that try to use a persisted query hash anyways.
async fn disabled_apq_request(
    request: SupergraphRequest,
) -> Result<SupergraphRequest, SupergraphResponse> {
    if request
        .supergraph_request
        .body()
        .extensions
        .contains_key("persistedQuery")
    {
        let errors = vec![
            crate::error::Error::builder()
                .message("PersistedQueryNotSupported".to_string())
                .locations(Default::default())
                .extension_code("PERSISTED_QUERY_NOT_SUPPORTED")
                .build(),
        ];
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
    use tower::ServiceExt;

    use super::*;
    use crate::Configuration;
    use crate::Context;
    use crate::assert_error_eq_ignoring_id;
    use crate::error::Error;
    use crate::services::router::ClientRequestAccepts;
    use crate::services::router::service::from_supergraph_mock_callback;
    use crate::services::router::service::from_supergraph_mock_callback_and_configuration;

    #[tokio::test]
    async fn it_works() {
        let hash = Cow::from("ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b38");
        let hash2 = hash.clone();

        let expected_apq_miss_error = Error::builder()
            .message("PersistedQueryNotFound".to_string())
            .locations(Default::default())
            .extension_code("PERSISTED_QUERY_NOT_FOUND")
            .build();

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
        let apq_response = router_service
            .ready()
            .await
            .expect("readied")
            .call(hash_only)
            .await
            .unwrap();

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

        assert_error_eq_ignoring_id!(expected_apq_miss_error, apq_error.errors[0]);

        let with_query = SupergraphRequest::fake_builder()
            .extension("persistedQuery", persisted.clone())
            .query("{__typename}".to_string())
            .context(new_context())
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();

        let full_response = router_service
            .ready()
            .await
            .expect("readied")
            .call(with_query)
            .await
            .unwrap();

        // the cache control header shouldn't have been tampered with
        assert!(
            full_response
                .response
                .headers()
                .get(CACHE_CONTROL)
                .is_none()
        );

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

        let apq_response = router_service
            .ready()
            .await
            .expect("readied")
            .call(second_hash_only)
            .await
            .unwrap();

        // the cache control header shouldn't have been tampered with
        assert!(apq_response.response.headers().get(CACHE_CONTROL).is_none());
    }

    #[tokio::test]
    async fn it_doesnt_update_the_cache_if_the_hash_is_not_valid() {
        let hash = Cow::from("ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b36");
        let hash2 = hash.clone();

        let expected_apq_miss_error = Error::builder()
            .message("PersistedQueryNotFound".to_string())
            .locations(Default::default())
            .extension_code("PERSISTED_QUERY_NOT_FOUND")
            .build();

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
            .ready()
            .await
            .expect("readied")
            .call(hash_only)
            .await
            .unwrap()
            .into_graphql_response_stream()
            .await
            .next()
            .await
            .unwrap()
            .unwrap();

        assert_error_eq_ignoring_id!(expected_apq_miss_error, apq_error.errors[0]);

        // sha256 is wrong, apq insert won't happen
        let insert_failed_response = router_service
            .ready()
            .await
            .expect("readied")
            .call(with_query)
            .await
            .unwrap();

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
        let expected_apq_insert_failed_error = Error::builder()
            .message("provided sha does not match query".to_string())
            .locations(Default::default())
            .extension_code("PERSISTED_QUERY_HASH_MISMATCH")
            .build();
        assert_error_eq_ignoring_id!(expected_apq_insert_failed_error, graphql_response.errors[0]);

        // apq insert failed, this call will miss
        let second_apq_error = router_service
            .ready()
            .await
            .expect("readied")
            .call(second_hash_only)
            .await
            .unwrap()
            .into_graphql_response_stream()
            .await
            .next()
            .await
            .unwrap()
            .unwrap();

        assert_error_eq_ignoring_id!(expected_apq_miss_error, second_apq_error.errors[0]);
    }

    #[tokio::test]
    async fn return_not_supported_when_disabled() {
        let expected_apq_miss_error = Error::builder()
            .message("PersistedQueryNotSupported".to_string())
            .locations(Default::default())
            .extension_code("PERSISTED_QUERY_NOT_SUPPORTED")
            .build();

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
        let apq_response = router_service
            .ready()
            .await
            .expect("readied")
            .call(hash_only)
            .await
            .unwrap();

        let apq_error = apq_response
            .into_graphql_response_stream()
            .await
            .next()
            .await
            .unwrap()
            .unwrap();

        assert_error_eq_ignoring_id!(expected_apq_miss_error, apq_error.errors[0]);

        let with_query = SupergraphRequest::fake_builder()
            .extension("persistedQuery", persisted.clone())
            .query("{__typename}".to_string())
            .context(new_context())
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();

        let with_query_response = router_service
            .ready()
            .await
            .expect("readied")
            .call(with_query)
            .await
            .unwrap();

        let apq_error = with_query_response
            .into_graphql_response_stream()
            .await
            .next()
            .await
            .unwrap()
            .unwrap();

        assert_error_eq_ignoring_id!(expected_apq_miss_error, apq_error.errors[0]);

        let without_apq = SupergraphRequest::fake_builder()
            .query("{__typename}".to_string())
            .context(new_context())
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();

        let without_apq_response = router_service
            .ready()
            .await
            .expect("readied")
            .call(without_apq)
            .await
            .unwrap();

        let without_apq_graphql_response = without_apq_response
            .into_graphql_response_stream()
            .await
            .next()
            .await
            .unwrap()
            .unwrap();

        assert!(without_apq_graphql_response.errors.is_empty());
    }

    fn new_context() -> Context {
        let context = Context::new();
        context.extensions().with_lock(|lock| {
            lock.insert(ClientRequestAccepts {
                json: true,
                ..Default::default()
            })
        });

        context
    }
}
