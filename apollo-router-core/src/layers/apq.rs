use crate::RouterRequest;
use moka::sync::Cache;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::task::Poll;
use tower::{Layer, Service};

#[derive(Deserialize, Clone, Debug)]
pub struct PersistedQuery {
    pub version: u8,
    #[serde(rename = "sha256Hash")]
    pub sha256hash: String,
}

pub struct APQ {
    cache: Cache<Vec<u8>, String>,
}

impl APQ {
    pub fn with_capacity(capacity: u64) -> Self {
        Self {
            cache: Cache::new(capacity),
        }
    }
}

pub struct APQService<S>
where
    S: Service<RouterRequest>,
{
    service: S,
    cache: Cache<Vec<u8>, String>,
}

impl<S> APQService<S>
where
    S: Service<RouterRequest>,
{
    pub fn new(service: S, capacity: u64) -> Self {
        Self {
            service,
            cache: Cache::new(capacity),
        }
    }
}

impl<S> Layer<S> for APQ
where
    S: Service<RouterRequest>,
{
    type Service = APQService<S>;

    fn layer(&self, service: S) -> Self::Service {
        APQService {
            cache: self.cache.clone(),
            service,
        }
    }
}

impl<S> Service<RouterRequest> for APQService<S>
where
    S: Service<RouterRequest>,
{
    type Response = <S as Service<RouterRequest>>::Response;

    type Error = <S as Service<RouterRequest>>::Error;

    type Future = <S as Service<RouterRequest>>::Future;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, mut req: RouterRequest) -> Self::Future {
        let cache = self.cache.clone();

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

            let graphql_request = req.http_request.body_mut();
            match (maybe_query_hash, graphql_request) {
                (Some(query_hash), graphql_request) if !graphql_request.query.is_empty() => {
                    if query_matches_hash(graphql_request.query.as_str(), query_hash.as_slice()) {
                        tracing::trace!("apq: cache insert");
                        cache.insert(query_hash, graphql_request.query.clone())
                    } else {
                        tracing::debug!("apq: graphql request doesn't match provided sha256Hash");
                    }
                }
                (Some(apq_hash), graphql_request) => {
                    if let Some(query) = cache.get(&apq_hash) {
                        tracing::trace!("apq: cache hit");
                        graphql_request.query = query;
                    } else {
                        tracing::trace!("apq: cache miss");
                    }
                }
                _ => {}
            }

            req
        };
        self.service.call(req)
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
    use crate::{Context, RouterResponse};
    use futures::{Future, FutureExt};
    use http::{Request, Response};
    use serde_json_bytes::Value;
    use std::{borrow::Cow, pin::Pin, sync::Arc};
    use tokio::sync::RwLock;
    use tower::{BoxError, ServiceExt};

    struct MockService<Req, Res> {
        nth_call: usize,
        mocks: Vec<Box<dyn Fn(Req) -> Result<Res, BoxError>>>,
    }

    impl<Req, Res> MockService<Req, Res> {
        pub fn new() -> Self {
            Self {
                nth_call: 0,
                mocks: Vec::new(),
            }
        }

        pub fn add_mock(mut self, mock: impl Fn(Req) -> Result<Res, BoxError> + 'static) -> Self {
            self.mocks.push(Box::new(mock));
            self
        }
    }

    impl<Req, Res> Service<Req> for MockService<Req, Res>
    where
        Res: Send + 'static,
    {
        type Response = Res;

        type Error = BoxError;

        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

        fn poll_ready(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: Req) -> Self::Future {
            let index = self.nth_call;
            self.nth_call += 1;
            let res = self.mocks[index](req);
            async move { res }.boxed()
        }
    }

    #[tokio::test]
    async fn it_works() {
        let hash = Cow::from("ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b38");
        let hash2 = hash.clone();
        let hash3 = hash.clone();

        let mock_service = MockService::<RouterRequest, RouterResponse>::new()
            // the first one should have lead to an APQ error
            // claiming the server doesn't have a query string for a given hash
            .add_mock(move |req: RouterRequest| {
                let as_json = req
                    .http_request
                    .body()
                    .extensions
                    .get("persistedQuery")
                    .unwrap();

                let persisted_query: PersistedQuery =
                    serde_json_bytes::from_value(as_json.clone()).unwrap();

                assert_eq!(persisted_query.sha256hash, hash);

                assert!(req.http_request.body().query.is_empty());

                Ok(RouterResponse {
                    response: Response::new(crate::Response {
                        label: Default::default(),
                        data: Value::Null,
                        path: Default::default(),
                        has_next: Default::default(),
                        errors: Default::default(),
                        extensions: Default::default(),
                    }),
                    context: Arc::new(RwLock::new(
                        Context::new().with_request(Arc::new(req.http_request)),
                    )),
                })
            })
            // the second one should have the right APQ header and the full query string
            .add_mock(move |req: RouterRequest| {
                let as_json = req
                    .http_request
                    .body()
                    .extensions
                    .get("persistedQuery")
                    .unwrap();

                let persisted_query: PersistedQuery =
                    serde_json_bytes::from_value(as_json.clone()).unwrap();

                assert_eq!(persisted_query.sha256hash, hash2);

                assert!(!req.http_request.body().query.is_empty());

                Ok(RouterResponse {
                    response: Response::new(crate::Response {
                        label: Default::default(),
                        data: Value::Null,
                        path: Default::default(),
                        has_next: Default::default(),
                        errors: Default::default(),
                        extensions: Default::default(),
                    }),
                    context: Arc::new(RwLock::new(
                        Context::new().with_request(Arc::new(req.http_request)),
                    )),
                })
            })
            // the second last one should have the right APQ header and the full query string
            // even though the query string wasn't provided by the client
            .add_mock(move |req: RouterRequest| {
                let as_json = req
                    .http_request
                    .body()
                    .extensions
                    .get("persistedQuery")
                    .unwrap();

                let persisted_query: PersistedQuery =
                    serde_json_bytes::from_value(as_json.clone()).unwrap();

                assert_eq!(persisted_query.sha256hash, hash3);

                assert!(!req.http_request.body().query.is_empty());

                let hash = hex::decode(hash3.as_bytes()).unwrap();

                assert!(query_matches_hash(
                    req.http_request.body().query.as_str(),
                    hash.as_slice()
                ));

                Ok(RouterResponse {
                    response: Response::new(crate::Response {
                        label: Default::default(),
                        data: Value::Null,
                        path: Default::default(),
                        has_next: Default::default(),
                        errors: Default::default(),
                        extensions: Default::default(),
                    }),
                    context: Arc::new(RwLock::new(
                        Context::new().with_request(Arc::new(req.http_request)),
                    )),
                })
            });

        // WOW :D
        let mut service_stack = APQ::with_capacity(1).layer(mock_service);

        let hash_only = RouterRequest {
            http_request: Request::new(crate::Request {
                query: Default::default(),
                operation_name: Default::default(),
                variables: Default::default(),
                extensions: serde_json::from_str(r#"{"persistedQuery":{"version":1,"sha256Hash":"ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b38"}}"#).unwrap(),
            }),
            context: Context::new(),
        };
        let hash_and_full_query = RouterRequest {
            http_request: Request::new(crate::Request {
                query: "{__typename}".to_string(),
                operation_name: Default::default(),
                variables: Default::default(),
                extensions: serde_json::from_str(r#"{"persistedQuery":{"version":1,"sha256Hash":"ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b38"}}"#).unwrap(),
            }),
            context: Context::new(),
        };

        // TODO: let's use an http::Request that implements clone or something
        let hash_only_again = RouterRequest {
            http_request: Request::new(crate::Request {
                query: Default::default(),
                operation_name: Default::default(),
                variables: Default::default(),
                extensions: serde_json::from_str(r#"{"persistedQuery":{"version":1,"sha256Hash":"ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b38"}}"#).unwrap(),
            }),
            context: Context::new(),
        };

        let services = service_stack.ready().await.unwrap();

        services.call(hash_only).await.unwrap();
        services.call(hash_and_full_query).await.unwrap();
        services.call(hash_only_again).await.unwrap();
    }
}
