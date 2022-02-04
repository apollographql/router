use futures::future::BoxFuture;
use moka::sync::Cache;
use std::hash::Hash;
use std::marker::PhantomData;
use std::task::Poll;
use tower::{Layer, Service};

// Demonstration caching layer
// Needs some work to make good. In particular Err responses from inner.call are not cached.
pub struct CachingService<S, Request, Key, Value, KeyFn, ValueFn, ResponseFn>
where
    Request: Send,
    S: Service<Request> + Send,
    Key: Send + Sync + Eq + Hash + Clone + 'static,
    Value: Send + Sync + Clone + 'static,
    KeyFn: Fn(&Request) -> Key + Clone + Send + 'static,
    ValueFn: Fn(&S::Response) -> Value + Clone + Send + 'static,
    ResponseFn: Fn(Request, Value) -> S::Response + Clone + Send + 'static,
    <S as Service<Request>>::Error: Send + Sync + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    key_fn: KeyFn,
    value_fn: ValueFn,
    response_fn: ResponseFn,
    inner: S,
    cache: Cache<Key, Value>,
    phantom: PhantomData<Request>,
}

impl<S, Request, Key, Value, KeyFn, ValueFn, ResponseFn> Service<Request>
    for CachingService<S, Request, Key, Value, KeyFn, ValueFn, ResponseFn>
where
    Request: Send,
    S: Service<Request> + Send,
    Key: Send + Sync + Eq + Hash + Clone + 'static,
    Value: Send + Sync + Clone + 'static,
    KeyFn: Fn(&Request) -> Key + Clone + Send + 'static,
    ValueFn: Fn(&S::Response) -> Value + Clone + Send + 'static,
    ResponseFn: Fn(Request, Value) -> S::Response + Clone + Send + 'static,
    <S as Service<Request>>::Error: Send + Sync,
    <S as Service<Request>>::Response: Send,
    <S as Service<Request>>::Future: Send,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let cache = self.cache.clone();
        let value_fn = self.value_fn.clone();
        let key = (self.key_fn)(&request);
        let value = self.cache.get(&key);
        match value {
            Some(value) => Box::pin(futures::future::ready(Ok((self.response_fn)(
                request, value,
            )))),
            None => {
                let delegate = self.inner.call(request);
                Box::pin(async move {
                    let response = delegate.await;
                    if let Ok(response) = &response {
                        let value = value_fn(response);
                        cache.insert(key, value);
                    }
                    response
                })
            }
        }
    }
}

pub struct CachingLayer<S, Request, Key, Value, KeyFn, ValueFn, ResponseFn>
where
    Request: Send,
    S: Service<Request> + Send,
    Key: Send + Sync + Eq + Hash + Clone + 'static,
    Value: Send + Sync + Clone + 'static,
    KeyFn: Fn(&Request) -> Key + Clone + Send + 'static,
    ValueFn: Fn(&S::Response) -> Value + Clone + Send + 'static,
    ResponseFn: Fn(Request, Value) -> S::Response + Clone + Send + 'static,
    <S as Service<Request>>::Error: Send + Sync + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    key_fn: KeyFn,
    value_fn: ValueFn,
    response_fn: ResponseFn,
    cache: Cache<Key, Value>,
    phantom1: PhantomData<Request>,
    phantom2: PhantomData<Value>,
    phantom3: PhantomData<S>,
}

impl<S, Request, Key, Value, KeyFn, ValueFn, ResponseFn>
    CachingLayer<S, Request, Key, Value, KeyFn, ValueFn, ResponseFn>
where
    Request: Send,
    S: Service<Request> + Send,
    Key: Send + Sync + Eq + Hash + Clone + 'static,
    Value: Send + Sync + Clone + 'static,
    KeyFn: Fn(&Request) -> Key + Clone + Send + 'static,
    ValueFn: Fn(&S::Response) -> Value + Clone + Send + 'static,
    ResponseFn: Fn(Request, Value) -> S::Response + Clone + Send + 'static,
    <S as Service<Request>>::Error: Send + Sync + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    pub fn new(
        cache: Cache<Key, Value>,
        key_fn: KeyFn,
        value_fn: ValueFn,
        response_fn: ResponseFn,
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

impl<S, Request, Key, Value, KeyFn, ValueFn, ResponseFn> Layer<S>
    for CachingLayer<S, Request, Key, Value, KeyFn, ValueFn, ResponseFn>
where
    Request: Send,
    S: Service<Request> + Send,
    Key: Send + Sync + Eq + Hash + Clone + 'static,
    Value: Send + Sync + Clone + 'static,
    KeyFn: Fn(&Request) -> Key + Clone + Send + 'static,
    ValueFn: Fn(&S::Response) -> Value + Clone + Send + 'static,
    ResponseFn: Fn(Request, Value) -> S::Response + Clone + Send + 'static,
    <S as Service<Request>>::Error: Send + Sync + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    type Service = CachingService<S, Request, Key, Value, KeyFn, ValueFn, ResponseFn>;

    fn layer(&self, inner: S) -> Self::Service {
        CachingService {
            key_fn: self.key_fn.clone(),
            value_fn: self.value_fn.clone(),
            response_fn: self.response_fn.clone(),
            inner,
            cache: self.cache.clone(),
            phantom: Default::default(),
        }
    }
}
