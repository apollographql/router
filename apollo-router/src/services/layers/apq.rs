//!  (A)utomatic (P)ersisted (Q)ueries cache.
//!
//!  For more information on APQ see:
//!  <https://www.apollographql.com/docs/apollo-server/performance/apq/>

use std::ops::ControlFlow;

use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json_bytes::json;
use serde_json_bytes::Value;
use sha2::Digest;
use sha2::Sha256;
use tower::buffer::Buffer;
use tower::BoxError;
use tower::Layer;
use tower::Service;

use crate::cache::DeduplicatingCache;
use crate::layers::async_checkpoint::AsyncCheckpointService;
use crate::layers::DEFAULT_BUFFER_SIZE;
use crate::RouterRequest;
use crate::RouterResponse;

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
    cache: DeduplicatingCache<Vec<u8>, String>,
}

impl APQLayer {
    pub(crate) fn with_cache(cache: DeduplicatingCache<Vec<u8>, String>) -> Self {
        Self { cache }
    }
}

impl<S> Layer<S> for APQLayer
where
    S: Service<RouterRequest, Response = RouterResponse, Error = BoxError> + Send + 'static,
    <S as Service<RouterRequest>>::Future: Send + 'static,
{
    type Service = AsyncCheckpointService<
        Buffer<S, RouterRequest>,
        BoxFuture<
            'static,
            Result<ControlFlow<<S as Service<RouterRequest>>::Response, RouterRequest>, BoxError>,
        >,
        RouterRequest,
    >;

    fn layer(&self, service: S) -> Self::Service {
        let cache = self.cache.clone();
        AsyncCheckpointService::new(
            move |mut req| {
                let cache = cache.clone();
                Box::pin(async move {
                    let maybe_query_hash: Option<Vec<u8>> = req
                        .originating_request
                        .body()
                        .extensions
                        .get("persistedQuery")
                        .and_then(|value| {
                            serde_json_bytes::from_value::<PersistedQuery>(value.clone()).ok()
                        })
                        .and_then(|persisted_query| {
                            hex::decode(persisted_query.sha256hash.as_bytes()).ok()
                        });

                    let body_query = req.originating_request.body().query.clone();

                    match (maybe_query_hash, body_query) {
                        (Some(query_hash), Some(query)) => {
                            if query_matches_hash(query.as_str(), query_hash.as_slice()) {
                                tracing::trace!("apq: cache insert");
                                let _ = req.context.insert("persisted_query_hit", false);
                                cache.insert(query_hash, query).await;
                            } else {
                                tracing::warn!(
                                    "apq: graphql request doesn't match provided sha256Hash"
                                );
                            }
                            Ok(ControlFlow::Continue(req))
                        }
                        (Some(apq_hash), _) => {
                            if let Ok(cached_query) = cache.get(&apq_hash).await.get().await {
                                let _ = req.context.insert("persisted_query_hit", true);
                                tracing::trace!("apq: cache hit");
                                req.originating_request.body_mut().query = Some(cached_query);
                                Ok(ControlFlow::Continue(req))
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
                                let res = RouterResponse::builder()
                                    .data(Value::default())
                                    .errors(errors)
                                    .context(req.context)
                                    .build()
                                    .expect("response is valid");

                                Ok(ControlFlow::Break(res))
                            }
                        }
                        _ => Ok(ControlFlow::Continue(req)),
                    }
                })
                    as BoxFuture<
                        'static,
                        Result<
                            ControlFlow<<S as Service<RouterRequest>>::Response, RouterRequest>,
                            BoxError,
                        >,
                    >
            },
            Buffer::new(service, DEFAULT_BUFFER_SIZE),
        )
    }
}

fn query_matches_hash(query: &str, hash: &[u8]) -> bool {
    let mut digest = Sha256::new();
    digest.update(query.as_bytes());
    hash == digest.finalize().as_slice()
}

#[cfg(test)]
mod apq_tests {
    use std::borrow::Cow;
    use std::collections::HashMap;

    use serde_json_bytes::json;
    use tower::ServiceExt;

    use super::*;
    use crate::error::Error;
    use crate::graphql::Response;
    use crate::plugin::test::MockRouterService;
    use crate::Context;

    #[tokio::test]
    async fn it_works() {
        let hash = Cow::from("ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b38");
        let hash2 = hash.clone();
        let hash3 = hash.clone();

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

        let mut mock_service = MockRouterService::new();
        // the first one should have lead to an APQ error
        // claiming the server doesn't have a query string for a given hash
        // it should have not been forwarded to our mock service

        // the second one should have the right APQ header and the full query string
        mock_service.expect_call().times(1).returning(move |req| {
            let body = req.originating_request.body();

            let as_json = body.extensions.get("persistedQuery").unwrap();

            let persisted_query: PersistedQuery =
                serde_json_bytes::from_value(as_json.clone()).unwrap();

            assert_eq!(persisted_query.sha256hash, hash2);

            assert!(body.query.is_some());

            Ok(RouterResponse::fake_builder()
                .build()
                .expect("expecting valid request"))
        });
        mock_service
            // the last one should have the right APQ header and the full query string
            // even though the query string wasn't provided by the client
            .expect_call()
            .times(1)
            .returning(move |req| {
                let body = req.originating_request.body();
                let as_json = body.extensions.get("persistedQuery").unwrap();

                let persisted_query: PersistedQuery =
                    serde_json_bytes::from_value(as_json.clone()).unwrap();

                assert_eq!(persisted_query.sha256hash, hash3);

                assert!(body.query.is_some());

                let hash = hex::decode(hash3.as_bytes()).unwrap();

                assert!(query_matches_hash(
                    body.query.clone().unwrap().as_str(),
                    hash.as_slice()
                ));

                Ok(RouterResponse::fake_builder()
                    .build()
                    .expect("expecting valid request"))
            });

        let mock = mock_service.build();

        let apq = APQLayer::with_cache(DeduplicatingCache::new().await);
        let mut service_stack = apq.layer(mock);

        let extensions = HashMap::from([(
            "persistedQuery".to_string(),
            json!({
                "version" : 1,
                "sha256Hash" : "ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b38"
            }),
        )]);

        let hash_only = RouterRequest::fake_builder()
            .extensions(extensions.clone())
            .build()
            .expect("expecting valid request");

        let second_hash_only = RouterRequest::fake_builder()
            .extensions(extensions.clone())
            .build()
            .expect("expecting valid request");

        let with_query = RouterRequest::fake_builder()
            .extensions(extensions)
            .query("{__typename}".to_string())
            .build()
            .expect("expecting valid request");

        let services = service_stack.ready().await.unwrap();
        let apq_error = services
            .call(hash_only)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        assert_error_matches(&expected_apq_miss_error, apq_error);

        let services = services.ready().await.unwrap();
        services.call(with_query).await.unwrap();

        let services = services.ready().await.unwrap();
        services.call(second_hash_only).await.unwrap();
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

        let mut mock_service_builder = MockRouterService::new();
        // the first one should have lead to an APQ error
        // claiming the server doesn't have a query string for a given hash
        // it should have not been forwarded to our mock service

        // the second one should have the right APQ header and the full query string
        mock_service_builder
            .expect_call()
            .times(1)
            .returning(move |req| {
                let body = req.originating_request.body();
                let as_json = body.extensions.get("persistedQuery").unwrap();

                let persisted_query: PersistedQuery =
                    serde_json_bytes::from_value(as_json.clone()).unwrap();

                assert_eq!(persisted_query.sha256hash, hash2);

                assert!(body.query.is_some());

                Ok(RouterResponse::fake_builder()
                    .build()
                    .expect("expecting valid request"))
            });

        // the last call should be an APQ error.
        // the provided hash was wrong, so the query wasn't inserted into the cache.

        let mock_service = mock_service_builder.build();

        let apq = APQLayer::with_cache(DeduplicatingCache::new().await);
        let mut service_stack = apq.layer(mock_service);

        let extensions = HashMap::from([(
            "persistedQuery".to_string(),
            json!({
                "version" : 1,
                "sha256Hash" : "ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b36"
            }),
        )]);

        let request_builder = RouterRequest::fake_builder().extensions(extensions.clone());

        let hash_only = request_builder
            .context(Context::new())
            .build()
            .expect("expecting valid request");

        let request_builder = RouterRequest::fake_builder().extensions(extensions.clone());

        let second_hash_only = request_builder
            .context(Context::new())
            .build()
            .expect("expecting valid request");

        let request_builder = RouterRequest::fake_builder().extensions(extensions);

        let with_query = request_builder
            .query("{__typename}".to_string())
            .context(Context::new())
            .build()
            .expect("expecting valid request");

        let services = service_stack.ready().await.unwrap();
        // This apq call will miss the APQ cache
        let apq_error = services
            .call(hash_only)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        assert_error_matches(&expected_apq_miss_error, apq_error);

        // sha256 is wrong, apq insert won't happen
        let services = services.ready().await.unwrap();
        services.call(with_query).await.unwrap();

        let services = services.ready().await.unwrap();

        // apq insert failed, this call will miss
        let second_apq_error = services
            .call(second_hash_only)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        assert_error_matches(&expected_apq_miss_error, second_apq_error);
    }

    fn assert_error_matches(expected_error: &Error, res: Response) {
        assert_eq!(&res.errors[0], expected_error);
    }
}
