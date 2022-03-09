use std::ops::ControlFlow;

use crate::{checkpoint::CheckpointService, plugin_utils, RouterRequest, RouterResponse};
use moka::sync::Cache;
use serde::Deserialize;
use serde_json_bytes::json;
use sha2::{Digest, Sha256};
use tower::{BoxError, Layer, Service};

#[derive(Deserialize, Clone, Debug)]
pub struct PersistedQuery {
    pub version: u8,
    #[serde(rename = "sha256Hash")]
    pub sha256hash: String,
}

#[derive(Clone)]
pub struct APQLayer {
    cache: Cache<Vec<u8>, String>,
}

impl APQLayer {
    pub fn with_cache(cache: Cache<Vec<u8>, String>) -> Self {
        Self { cache }
    }
}

impl Default for APQLayer {
    fn default() -> Self {
        Self::with_cache(Cache::new(512))
    }
}

impl<S> Layer<S> for APQLayer
where
    S: Service<RouterRequest, Response = RouterResponse> + Send + 'static,
    <S as Service<RouterRequest>>::Future: Send + 'static,
    <S as Service<RouterRequest>>::Error: Into<BoxError> + Send + 'static,
{
    type Service = CheckpointService<S, RouterRequest>;

    fn layer(&self, service: S) -> Self::Service {
        let cache = self.cache.clone();
        CheckpointService::new(
            move |mut req| {
                let maybe_query_hash: Option<Vec<u8>> = req
                    .context
                    .request
                    .body()
                    .extensions
                    .get("persistedQuery")
                    .and_then(|value| {
                        serde_json_bytes::from_value::<PersistedQuery>(value.clone()).ok()
                    })
                    .and_then(|persisted_query| {
                        hex::decode(persisted_query.sha256hash.as_bytes()).ok()
                    });

                let body_query = req.context.request.body().query.clone();

                match (maybe_query_hash, body_query) {
                    (Some(query_hash), Some(query)) => {
                        if query_matches_hash(query.as_str(), query_hash.as_slice()) {
                            tracing::trace!("apq: cache insert");
                            cache.insert(query_hash, query);
                        } else {
                            tracing::warn!(
                                "apq: graphql request doesn't match provided sha256Hash"
                            );
                        }
                        Ok(ControlFlow::Continue(req))
                    }
                    (Some(apq_hash), _) => {
                        if let Some(cached_query) = cache.get(&apq_hash) {
                            tracing::trace!("apq: cache hit");
                            req.context.request.body_mut().query = Some(cached_query);
                            Ok(ControlFlow::Continue(req))
                        } else {
                            tracing::trace!("apq: cache miss");
                            let res = plugin_utils::RouterResponse::builder()
                                .errors(vec![crate::Error {
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
                                }])
                                .context(req.context.into())
                                .build()
                                .into();

                            Ok(ControlFlow::Break(res))
                        }
                    }
                    _ => Ok(ControlFlow::Continue(req)),
                }
            },
            service,
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
    use super::*;
    use crate::{
        plugin_utils::{MockRouterService, RouterRequest, RouterResponse},
        ResponseBody,
    };
    use serde_json_bytes::json;
    use std::borrow::Cow;
    use tower::ServiceExt;

    #[tokio::test]
    async fn it_works() {
        let hash = Cow::from("ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b38");
        let hash2 = hash.clone();
        let hash3 = hash.clone();

        let expected_apq_miss_error = crate::Error {
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
            let body = req.context.request.body();

            let as_json = body.extensions.get("persistedQuery").unwrap();

            let persisted_query: PersistedQuery =
                serde_json_bytes::from_value(as_json.clone()).unwrap();

            assert_eq!(persisted_query.sha256hash, hash2);

            assert!(body.query.is_some());

            Ok(RouterResponse::builder().build().into())
        });
        mock_service
            // the last one should have the right APQ header and the full query string
            // even though the query string wasn't provided by the client
            .expect_call()
            .times(1)
            .returning(move |req| {
                let body = req.context.request.body();
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

                Ok(RouterResponse::builder().build().into())
            });

        let mock = mock_service.build();

        let mut service_stack = APQLayer::default().layer(mock);

        let request_builder = RouterRequest::builder().extensions(vec![(
            "persistedQuery",
            json!({
                "version" : 1,
                "sha256Hash" : "ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b38"
            }),
        )]);

        let hash_only = request_builder.clone().build();

        let second_hash_only = request_builder.clone().build();

        let with_query = request_builder.query("{__typename}".to_string()).build();

        let services = service_stack.ready().await.unwrap();
        let apq_error = services.call(hash_only.into()).await.unwrap();

        assert_error_matches(&expected_apq_miss_error, apq_error);

        let services = services.ready().await.unwrap();
        services.call(with_query.into()).await.unwrap();

        let services = services.ready().await.unwrap();
        services.call(second_hash_only.into()).await.unwrap();
    }

    #[tokio::test]
    async fn it_doesnt_update_the_cache_if_the_hash_is_not_valid() {
        let hash = Cow::from("ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b36");
        let hash2 = hash.clone();
        let hash3 = hash.clone();

        let expected_apq_miss_error = crate::Error {
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
                let body = req.context.request.body();
                let as_json = body.extensions.get("persistedQuery").unwrap();

                let persisted_query: PersistedQuery =
                    serde_json_bytes::from_value(as_json.clone()).unwrap();

                assert_eq!(persisted_query.sha256hash, hash2);

                assert!(body.query.is_some());

                Ok(RouterResponse::builder().build().into())
            });
        mock_service_builder
            // the second last one should have the right APQ header and the full query string
            // even though the query string wasn't provided by the client
            .expect_call()
            .times(1)
            .returning(move |req| {
                let body = req.context.request.body();

                let as_json = body.extensions.get("persistedQuery").unwrap();

                let persisted_query: PersistedQuery =
                    serde_json_bytes::from_value(as_json.clone()).unwrap();

                assert_eq!(persisted_query.sha256hash, hash3);

                assert!(body.query.is_some());

                let hash = hex::decode(hash3.as_bytes()).unwrap();

                assert!(!query_matches_hash(
                    body.query.clone().unwrap_or_default().as_str(),
                    hash.as_slice()
                ));

                Ok(RouterResponse::builder().build().into())
            });

        let mock_service = mock_service_builder.build();

        let mut service_stack = APQLayer::default().layer(mock_service);

        let request_builder = RouterRequest::builder().extensions(vec![(
            "persistedQuery",
            json!({
                "version" : 1,
                "sha256Hash" : "ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b36"
            }),
        )]);

        let hash_only = request_builder.clone().build();
        let second_hash_only = request_builder.clone().build();
        let with_query = request_builder.query("{__typename}".to_string()).build();

        let services = service_stack.ready().await.unwrap();
        // This apq call will miss the APQ cache
        let apq_error = services.call(hash_only.into()).await.unwrap();

        assert_error_matches(&expected_apq_miss_error, apq_error);

        // sha256 is wrong, apq insert won't happen
        let services = services.ready().await.unwrap();
        services.call(with_query.into()).await.unwrap();

        let services = services.ready().await.unwrap();

        // apq insert failed, this call will miss
        let second_apq_error = services.call(second_hash_only.into()).await.unwrap();

        assert_error_matches(&expected_apq_miss_error, second_apq_error);
    }

    fn assert_error_matches(expected_error: &crate::Error, res: crate::RouterResponse) {
        if let ResponseBody::GraphQL(graphql_response) = res.response.body() {
            assert_eq!(&graphql_response.errors[0], expected_error);
        } else {
            panic!("expected a graphql response");
        }
    }
}
