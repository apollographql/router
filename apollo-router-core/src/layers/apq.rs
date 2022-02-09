use crate::{test_utils::structures::RouterResponseBuilder, RouterRequest, RouterResponse};
use futures::future::BoxFuture;
use moka::sync::Cache;
use serde::Deserialize;
use serde_json_bytes::json;
use sha2::{Digest, Sha256};
use std::task::Poll;
use tower::{BoxError, Layer, Service};

#[derive(Deserialize, Clone, Debug)]
pub struct PersistedQuery {
    pub version: u8,
    #[serde(rename = "sha256Hash")]
    pub sha256hash: String,
}

#[derive(Clone)]
pub struct APQ {
    cache: Cache<Vec<u8>, String>,
    response_builder: RouterResponseBuilder,
}

impl APQ {
    pub fn with_capacity(capacity: u64) -> Self {
        Self {
            cache: Cache::new(capacity),
            response_builder: RouterResponseBuilder::new().push_error(crate::Error {
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
            }),
        }
    }
}
pub struct APQService<S>
where
    S: Service<RouterRequest>,
{
    service: S,
    apq: APQ,
}

impl<S> APQService<S>
where
    S: Service<RouterRequest>,
{
    pub fn new(service: S, capacity: u64) -> Self {
        Self {
            service,
            apq: APQ::with_capacity(capacity),
        }
    }
}

impl<S> Layer<S> for APQ
where
    S: Service<RouterRequest, Response = RouterResponse>,
{
    type Service = APQService<S>;

    fn layer(&self, service: S) -> Self::Service {
        APQService {
            apq: self.clone(),
            service,
        }
    }
}

impl<S> Service<RouterRequest> for APQService<S>
where
    S: Service<RouterRequest, Response = RouterResponse, Error = BoxError>,
    S::Future: Send + 'static,
{
    type Response = <S as Service<RouterRequest>>::Response;

    type Error = <S as Service<RouterRequest>>::Error;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, mut req: RouterRequest) -> Self::Future {
        let apq = self.apq.clone();

        let req = {
            let maybe_query_hash: Option<Vec<u8>> = req
                .http_request
                .body()
                .extensions
                .get("persistedQuery")
                .and_then(|value| {
                    serde_json_bytes::from_value::<PersistedQuery>(value.clone()).ok()
                })
                .and_then(|persisted_query| {
                    hex::decode(persisted_query.sha256hash.as_bytes()).ok()
                });

            let body_query = req.http_request.body().query.clone();

            match (maybe_query_hash, body_query) {
                (Some(query_hash), Some(query)) => {
                    if query_matches_hash(query.as_str(), query_hash.as_slice()) {
                        tracing::trace!("apq: cache insert");
                        apq.cache.insert(query_hash, query);
                    } else {
                        tracing::warn!("apq: graphql request doesn't match provided sha256Hash");
                    }
                }
                (Some(apq_hash), _) => {
                    if let Some(cached_query) = apq.cache.get(&apq_hash) {
                        tracing::trace!("apq: cache hit");
                        req.http_request.body_mut().query = Some(cached_query);
                    } else {
                        tracing::trace!("apq: cache miss");
                        let res = apq
                            .response_builder
                            .with_context(req.context.with_request(req.http_request.into()))
                            .build();
                        return Box::pin(async move { Ok(res) });
                    }
                }
                _ => {}
            }
            // A query must be available at this point
            if req.http_request.body().query.is_none()
                || req.http_request.body().query == Some("".to_string())
            {
                let res = RouterResponseBuilder::new()
                    .push_error(crate::Error {
                        message: "Must provide query string.".to_string(),
                        locations: Default::default(),
                        path: Default::default(),
                        extensions: Default::default(),
                    })
                    .with_context(req.context.with_request(req.http_request.into()))
                    .build();
                return Box::pin(async move { Ok(res) });
            }
            req
        };
        Box::pin(self.service.call(req))
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
        test_utils::{structures::RouterResponseBuilder, MockRouterService, RouterRequestBuilder},
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
        mock_service
            .expect_call()
            .times(1)
            .returning(move |req: RouterRequest| {
                let as_json = req
                    .http_request
                    .body()
                    .extensions
                    .get("persistedQuery")
                    .unwrap();

                let persisted_query: PersistedQuery =
                    serde_json_bytes::from_value(as_json.clone()).unwrap();

                assert_eq!(persisted_query.sha256hash, hash2);

                assert!(req.http_request.body().query.is_some());

                Ok(RouterResponseBuilder::new().build())
            });
        mock_service
            // the second last one should have the right APQ header and the full query string
            // even though the query string wasn't provided by the client
            .expect_call()
            .times(1)
            .returning(move |req: RouterRequest| {
                let as_json = req
                    .http_request
                    .body()
                    .extensions
                    .get("persistedQuery")
                    .unwrap();

                let persisted_query: PersistedQuery =
                    serde_json_bytes::from_value(as_json.clone()).unwrap();

                assert_eq!(persisted_query.sha256hash, hash3);

                assert!(req.http_request.body().query.is_some());

                let hash = hex::decode(hash3.as_bytes()).unwrap();

                assert!(query_matches_hash(
                    req.http_request.body().query.clone().unwrap().as_str(),
                    hash.as_slice()
                ));

                Ok(RouterResponseBuilder::new().build())
            });

        let mock = mock_service.build();

        let mut service_stack = APQ::with_capacity(1).layer(mock);

        let request_builder = RouterRequestBuilder::new().with_named_extension(
            "persistedQuery",
            json!({
                "version" : 1,
                "sha256Hash" : "ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b38"
            }),
        );

        let hash_only = request_builder.build();

        let second_hash_only = request_builder.build();

        let with_query = request_builder.with_query("{__typename}").build();

        let services = service_stack.ready().await.unwrap();
        let apq_error = services.call(hash_only).await.unwrap();

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
            .returning(move |req: RouterRequest| {
                let as_json = req
                    .http_request
                    .body()
                    .extensions
                    .get("persistedQuery")
                    .unwrap();

                let persisted_query: PersistedQuery =
                    serde_json_bytes::from_value(as_json.clone()).unwrap();

                assert_eq!(persisted_query.sha256hash, hash2);

                assert!(req.http_request.body().query.is_some());

                Ok(RouterResponseBuilder::new().build())
            });
        mock_service_builder
            // the second last one should have the right APQ header and the full query string
            // even though the query string wasn't provided by the client
            .expect_call()
            .times(1)
            .returning(move |req: RouterRequest| {
                let as_json = req
                    .http_request
                    .body()
                    .extensions
                    .get("persistedQuery")
                    .unwrap();

                let persisted_query: PersistedQuery =
                    serde_json_bytes::from_value(as_json.clone()).unwrap();

                assert_eq!(persisted_query.sha256hash, hash3);

                assert!(req.http_request.body().query.is_some());

                let hash = hex::decode(hash3.as_bytes()).unwrap();

                assert!(!query_matches_hash(
                    req.http_request
                        .body()
                        .query
                        .clone()
                        .unwrap_or_default()
                        .as_str(),
                    hash.as_slice()
                ));

                Ok(RouterResponseBuilder::new().build())
            });

        let mock_service = mock_service_builder.build();

        let mut service_stack = APQ::with_capacity(1).layer(mock_service);

        let request_builder = RouterRequestBuilder::new().with_named_extension(
            "persistedQuery",
            json!({
                "version" : 1,
                "sha256Hash" : "ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b36"
            }),
        );

        let hash_only = request_builder.build();
        let second_hash_only = request_builder.build();
        let with_query = request_builder.with_query("{__typename}").build();

        let services = service_stack.ready().await.unwrap();
        // This apq call will miss
        let apq_error = services.call(hash_only).await.unwrap();

        assert_error_matches(&expected_apq_miss_error, apq_error);

        // sha256 is wrong, apq insert won't happen
        let services = services.ready().await.unwrap();
        services.call(with_query).await.unwrap();

        let services = services.ready().await.unwrap();

        // apq insert failed, this call will miss
        let second_apq_error = services.call(second_hash_only).await.unwrap();

        assert_error_matches(&expected_apq_miss_error, second_apq_error);
    }

    #[tokio::test]
    async fn it_will_error_on_empty_query_and_no_apq_header() {
        let expected_error = crate::Error {
            message: "Must provide query string.".to_string(),
            locations: Default::default(),
            path: Default::default(),
            extensions: Default::default(),
        };

        let mock_service = MockRouterService::new().build();

        let mut service_stack = APQ::with_capacity(1).layer(mock_service);

        let empty_request = RouterRequestBuilder::new().build();

        let services = service_stack.ready().await.unwrap();

        let actual_response = services.call(empty_request).await.unwrap();

        assert_error_matches(&expected_error, actual_response);
    }

    fn assert_error_matches(expected_error: &crate::Error, response: RouterResponse) {
        if let ResponseBody::GraphQL(graphql_response) = response.response.body() {
            assert_eq!(&graphql_response.errors[0], expected_error);
        } else {
            panic!("expected a graphql response");
        }
    }
}
