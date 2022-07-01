//! Provides a CachingLayer used to implement the cache functionality of [`crate::layers::ServiceBuilderExt`].

use std::hash::Hash;
use std::marker::PhantomData;
use std::sync::Arc;
use std::task::Poll;

use futures::future::BoxFuture;
use futures::FutureExt;
use futures::TryFutureExt;
use moka::sync::Cache;
use tokio::sync::RwLock;
use tower::BoxError;
use tower::Layer;
use tower::Service;

type Sentinel<Value> = Arc<RwLock<Option<Value>>>;

/// [`Service`] for cache.
pub struct CachingService<S, Request, Key, Value>
where
    Request: Send,
    S: Service<Request> + Send,
    <S as Service<Request>>::Error: Into<BoxError>,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    key_fn: fn(&Request) -> Option<&Key>,
    value_fn: fn(&S::Response) -> &Value,
    response_fn: fn(Request, Value) -> S::Response,
    inner: S,
    cache: Cache<Key, Sentinel<Result<Value, String>>>,
    phantom: PhantomData<Request>,
}

impl<S, Request, Key, Value> Service<Request> for CachingService<S, Request, Key, Value>
where
    Request: Send + 'static,
    S: Service<Request> + Send + 'static,
    Key: Send + Sync + Eq + Hash + Clone + 'static,
    Value: Send + Sync + Clone + 'static,
    <S as Service<Request>>::Error: Into<BoxError> + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let key = (self.key_fn)(&request);
        match key {
            None => {
                // The key function returned none, so the request was uncachable.
                self.inner.call(request).err_into().boxed()
            }
            Some(key) => {
                let value = self.cache.get(key);
                match value {
                    None => {
                        // This is a new value. Create a sentinel and lock it.
                        // The sentinel is an RWLock over an initially empty option.
                        // Once the sentinel is created it is locked it so that readers must wait to read.
                        // The sentinel is then put into the cache so that any subsequent read
                        // will pick this up and have to wait.
                        let sentinel = Arc::new(RwLock::new(None));
                        let mut guard = sentinel
                            .clone()
                            .try_write_owned()
                            .expect("This will never fail as we have just created the lock.");

                        self.cache.insert(key.clone(), sentinel);

                        let value_fn = self.value_fn;
                        self.inner
                            .call(request)
                            .err_into()
                            .then(move |response| async move {
                                // We got the response now. Update the sentinel and release the guard.
                                let sentinel_value = match &response {
                                    Ok(response) => Ok((value_fn)(response).clone()),
                                    Err(e) => Err(e.to_string()),
                                };
                                let _ = guard.insert(sentinel_value);
                                // Here we could also add functionality to immediately invalidate the key
                                // This would have the effect of only caching for the duration of the
                                // downstream call.
                                response
                            })
                            .boxed()
                    }
                    Some(value) => {
                        // The value has been populated, we need to await on the sentinel.
                        // The read lock on the sentinel is released when it is populated allowing the
                        // current read to proceed.
                        let response_fn = self.response_fn;
                        value
                            .read_owned()
                            .map(move |value| {
                                let value = value.clone().expect("Value will always have been set");
                                match value {
                                    Ok(value) => Ok((response_fn)(request, value)),
                                    Err(err) => Err(err.into()),
                                }
                            })
                            .boxed()
                    }
                }
            }
        }
    }
}

/// [`Layer`] for cache.
pub struct CachingLayer<Request, Response, Key, Value>
where
    Request: Send,
{
    key_fn: fn(&Request) -> Option<&Key>,
    value_fn: fn(&Response) -> &Value,
    response_fn: fn(Request, Value) -> Response,
    cache: Cache<Key, crate::layers::cache::Sentinel<Result<Value, String>>>,
}

impl<Request, Response, Key, Value> CachingLayer<Request, Response, Key, Value>
where
    Request: Send,
{
    pub fn new(
        cache: Cache<Key, Sentinel<Result<Value, String>>>,
        key_fn: fn(&Request) -> Option<&Key>,
        value_fn: fn(&Response) -> &Value,
        response_fn: fn(Request, Value) -> Response,
    ) -> Self {
        Self {
            key_fn,
            value_fn,
            response_fn,
            cache,
        }
    }
}

impl<S, Request, Key, Value> Layer<S> for CachingLayer<Request, S::Response, Key, Value>
where
    Request: Send,
    S: Service<Request> + Send,
    Key: Send + Sync + Eq + Hash + Clone + 'static,
    Value: Send + Sync + Clone + 'static,
    <S as Service<Request>>::Error: Into<BoxError>,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    type Service = CachingService<S, Request, Key, Value>;

    fn layer(&self, inner: S) -> Self::Service {
        CachingService {
            key_fn: self.key_fn,
            value_fn: self.value_fn,
            response_fn: self.response_fn,
            inner,
            cache: self.cache.clone(),
            phantom: Default::default(),
        }
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use mockall::predicate::eq;
    use moka::sync::CacheBuilder;
    use tower::filter::AsyncPredicate;
    use tower::BoxError;
    use tower::ServiceBuilder;
    use tower::ServiceExt;

    use super::*;
    use crate::mock_service;

    #[derive(Default, Clone)]
    struct Slow;

    impl AsyncPredicate<A> for Slow {
        type Future = BoxFuture<'static, Result<A, BoxError>>;
        type Request = A;

        fn check(&mut self, request: A) -> Self::Future {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                Ok(request)
            })
        }
    }

    #[derive(Clone, Eq, PartialEq, Debug)]
    #[allow(unreachable_pub)]
    pub struct A {
        key: String,
        value: String,
    }

    #[derive(Clone, Eq, PartialEq, Debug)]
    #[allow(unreachable_pub)]
    pub struct B {
        key: String,
        value: String,
    }

    mock_service!(AB, A, B);

    #[tokio::test]
    async fn is_should_cache_ok() {
        let mut mock_service = MockABService::new();

        mock_service.expect_call().times(1).returning(move |a| {
            Ok(B {
                key: a.key,
                value: "there".into(),
            })
        });

        let mut service = create_service(mock_service);

        let expected = Ok(B {
            key: "Cacheable hi".to_string(),
            value: "there".to_string(),
        });

        let b = service
            .ready()
            .await
            .unwrap()
            .call(A {
                key: "Cacheable hi".into(),
                value: "there".into(),
            })
            .await;
        assert_eq!(b, expected);
        let b = service
            .ready()
            .await
            .unwrap()
            .call(A {
                key: "Cacheable hi".into(),
                value: "there".into(),
            })
            .await;
        assert_eq!(b, expected);
    }

    #[tokio::test]
    async fn is_should_cache_err() {
        let mut mock_service = MockABService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |a| Err(BoxError::from(format!("{} err", a.key))));

        let mut service = create_service(mock_service);

        let expected = Err("Cacheable hi err".to_string());

        let b = service
            .ready()
            .await
            .unwrap()
            .call(A {
                key: "Cacheable hi".into(),
                value: "there".into(),
            })
            .await;
        assert_eq!(b, expected);
        let b = service
            .ready()
            .await
            .unwrap()
            .call(A {
                key: "Cacheable hi".into(),
                value: "there".into(),
            })
            .await;
        assert_eq!(b, expected);
    }

    #[tokio::test]
    async fn it_should_not_cache_non_cachable() {
        let mut mock_service = MockABService::new();

        mock_service
            .expect_call()
            .times(2)
            .with(eq(A {
                key: "Not cacheable".into(),
                value: "Needed".into(),
            }))
            .returning(move |a| {
                Ok(B {
                    key: a.key,
                    value: "there".into(),
                })
            });

        let mut service = create_service(mock_service);

        for _ in 0..2 {
            service
                .ready()
                .await
                .unwrap()
                .call(A {
                    key: "Not cacheable".into(),
                    value: "Needed".into(),
                })
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn it_should_dedupe_in_flight_calls() {
        let mut mock_service = MockABService::new();

        mock_service
            .expect_call()
            .times(1)
            .with(eq(A {
                key: "Cacheable A".into(),
                value: "Not needed".into(),
            }))
            .returning(move |a| {
                Ok(B {
                    key: a.key,
                    value: "there".into(),
                })
            });

        let mut service = create_service(mock_service);

        let mut tasks = Vec::default();
        // Our service is a little slow. It will pause for 10ms.
        // This is enough time for our other requests to back up and enter the dedup logic.
        // Once the first request returns the rest will all return.
        for _ in 0..10 {
            tasks.push(service.ready().await.unwrap().call(A {
                key: "Cacheable A".into(),
                value: "Not needed".into(),
            }));
        }

        let results = futures::future::join_all(tasks).await;
        for result in results {
            assert_eq!(
                result,
                Ok(B {
                    key: "Cacheable A".into(),
                    value: "there".into()
                })
            );
        }
    }

    fn create_service(
        mock_service: MockABService,
    ) -> impl Service<A, Response = B, Error = String> {
        let cache = CacheBuilder::new(2).build();
        ServiceBuilder::new()
            .layer(CachingLayer::new(
                cache,
                |request: &A| {
                    if request.key.starts_with("Cacheable") {
                        Some(&request.key)
                    } else {
                        None
                    }
                },
                |request: &B| &request.value,
                |request: A, value: String| B {
                    key: request.key,
                    value,
                },
            ))
            .filter_async(Slow::default())
            .service(mock_service.build())
            .map_err(|e: BoxError| e.to_string())
    }
}
