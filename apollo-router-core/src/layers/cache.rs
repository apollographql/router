use futures::future::BoxFuture;
use futures::TryFutureExt;
use moka::sync::Cache;
use std::hash::Hash;
use std::marker::PhantomData;
use std::task::Poll;
use tower::{BoxError, Layer, Service};

pub struct CachingService<S, Request, Key, Value>
where
    Request: Send,
    S: Service<Request> + Send,
    <S as Service<Request>>::Error: Into<BoxError>,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    key_fn: fn(&Request) -> Key,
    value_fn: fn(&S::Response) -> Value,
    response_fn: fn(Request, Value) -> S::Response,
    inner: S,
    cache: Cache<Key, Result<Value, String>>,
    phantom: PhantomData<Request>,
}

impl<S, Request, Key, Value> Service<Request> for CachingService<S, Request, Key, Value>
where
    Request: Send,
    S: Service<Request> + Send,
    Key: Send + Sync + Eq + Hash + Clone + 'static,
    Value: Send + Sync + Clone + 'static,
    <S as Service<Request>>::Error: Into<BoxError>,
    <S as Service<Request>>::Response: Send,
    <S as Service<Request>>::Future: Send,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let cache = self.cache.clone();
        let value_fn = self.value_fn;
        let key = (self.key_fn)(&request);
        let value = self.cache.get(&key);
        match value {
            Some(Ok(value)) => Box::pin(futures::future::ready(Ok((self.response_fn)(
                request, value,
            )))),
            Some(Err(err)) => Box::pin(futures::future::ready(Err(BoxError::from(err)))),
            None => {
                let delegate = self.inner.call(request).err_into();
                Box::pin(async move {
                    let response = delegate.await;

                    match &response {
                        Ok(result) => {
                            let value = value_fn(result);
                            cache.insert(key, Ok(value));
                        }
                        Err(err) => {
                            cache.insert(key, Err(err.to_string()));
                        }
                    }
                    response
                })
            }
        }
    }
}

pub struct CachingLayer<S, Request, Key, Value>
where
    Request: Send,
    S: Service<Request> + Send,
    <S as Service<Request>>::Error: Into<BoxError>,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    key_fn: fn(&Request) -> Key,
    value_fn: fn(&S::Response) -> Value,
    response_fn: fn(Request, Value) -> S::Response,
    cache: Cache<Key, Result<Value, String>>,
    phantom1: PhantomData<Request>,
    phantom2: PhantomData<Value>,
    phantom3: PhantomData<S>,
}

impl<S, Request, Key, Value> CachingLayer<S, Request, Key, Value>
where
    Request: Send,
    S: Service<Request> + Send,
    <S as Service<Request>>::Error: Into<BoxError>,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    pub fn new(
        cache: Cache<Key, Result<Value, String>>,
        key_fn: fn(&Request) -> Key,
        value_fn: fn(&S::Response) -> Value,
        response_fn: fn(Request, Value) -> S::Response,
    ) -> Self {
        Self {
            key_fn,
            value_fn,
            response_fn,
            cache,
            phantom1: Default::default(),
            phantom2: Default::default(),
            phantom3: Default::default(),
        }
    }
}

impl<S, Request, Key, Value> Layer<S> for CachingLayer<S, Request, Key, Value>
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
    use super::*;
    use crate::mock_service;
    use moka::sync::CacheBuilder;
    use tower::{BoxError, ServiceBuilder, ServiceExt};

    #[derive(Clone, Eq, PartialEq, Debug)]
    pub struct A {
        key: String,
    }

    #[derive(Clone, Eq, PartialEq, Debug)]
    pub struct B {
        key: String,
        value: String,
    }

    mock_service!(AB, A, B);

    #[tokio::test]
    async fn cache_ok() {
        let mut mock_service = MockABService::new();

        mock_service.expect_call().times(1).returning(move |a| {
            Ok(B {
                key: a.key,
                value: "there".into(),
            })
        });

        let mut service = create_service(mock_service);

        let expected = Ok(B {
            key: "hi".to_string(),
            value: "there".to_string(),
        });

        let b = service
            .ready()
            .await
            .unwrap()
            .call(A { key: "hi".into() })
            .await;
        assert_eq!(b, expected);
        let b = service
            .ready()
            .await
            .unwrap()
            .call(A { key: "hi".into() })
            .await;
        assert_eq!(b, expected);
    }

    #[tokio::test]
    async fn cache_err() {
        let mut mock_service = MockABService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |a| Err(BoxError::from(format!("{} err", a.key))));

        let mut service = create_service(mock_service);

        let expected = Err("hi err".to_string());

        let b = service
            .ready()
            .await
            .unwrap()
            .call(A { key: "hi".into() })
            .await;
        assert_eq!(b, expected);
        let b = service
            .ready()
            .await
            .unwrap()
            .call(A { key: "hi".into() })
            .await;
        assert_eq!(b, expected);
    }

    fn create_service(
        mock_service: MockABService,
    ) -> impl Service<A, Response = B, Error = String> {
        let cache = CacheBuilder::new(2).build();
        ServiceBuilder::new()
            .layer(CachingLayer::new(
                cache,
                |r: &A| r.key.clone(),
                |r: &B| r.value.clone(),
                |r: A, c: String| B {
                    key: r.key,
                    value: c,
                },
            ))
            .service(mock_service.build())
            .map_err(|e: BoxError| e.to_string())
    }
}
