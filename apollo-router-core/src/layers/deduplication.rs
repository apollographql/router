use futures::FutureExt;
use futures::TryFutureExt;
use futures::{future::BoxFuture, lock::Mutex};
use std::hash::Hash;
use std::{collections::HashMap, sync::Arc, task::Poll};
use tokio::sync::broadcast;
use tokio::sync::broadcast::Sender;
use tower::{BoxError, Layer};
use tower_service::Service;

pub struct DeduplicationLayer<S, Request, Key>
where
    Request: Send,
    Key: Eq + Hash + Send,
    S: Service<Request> + Send + Clone,
    <S as Service<Request>>::Response: Clone + Send,
    <S as Service<Request>>::Error: Into<BoxError> + Send + Sync,
    <S as Service<Request>>::Future: Send,
{
    key_fn: fn(&Request) -> Option<&Key>,
    request_fn: fn(&Key) -> Request,
    merge_fn: fn(Request, S::Response) -> S::Response,
}

impl<S, Request, Key> DeduplicationLayer<S, Request, Key>
where
    Request: Send,
    Key: Eq + Hash + Send,
    S: Service<Request> + Send + Clone,
    <S as Service<Request>>::Response: Clone + Send,
    <S as Service<Request>>::Error: Into<BoxError> + Send + Sync,
    <S as Service<Request>>::Future: Send,
{
    pub fn new(
        key_fn: fn(&Request) -> Option<&Key>,
        request_fn: fn(&Key) -> Request,
        merge_fn: fn(Request, S::Response) -> S::Response,
    ) -> Self {
        Self {
            key_fn,
            request_fn,
            merge_fn,
        }
    }
}

impl<S, Request, Key> Layer<S> for DeduplicationLayer<S, Request, Key>
where
    Request: Send + 'static,
    Key: Clone + Eq + Hash + Send + 'static,
    S: Service<Request> + Send + Clone,
    <S as Service<Request>>::Response: Clone + Send,
    <S as Service<Request>>::Error: Into<BoxError> + Send + Sync,
    <S as Service<Request>>::Future: Send,
{
    type Service = DeduplicationService<S, Request, Key>;

    fn layer(&self, service: S) -> Self::Service {
        DeduplicationService::new(service, self.key_fn, self.request_fn, self.merge_fn)
    }
}

pub struct DeduplicationService<S, Request, Key>
where
    Request: Send + 'static,
    Key: Clone + Eq + Hash + Send + 'static,
    S: Service<Request> + Send + Clone,
    <S as Service<Request>>::Response: Clone + Send,
    <S as Service<Request>>::Error: Into<BoxError> + Send + Sync,
    <S as Service<Request>>::Future: Send,
{
    service: S,
    ready_service: Option<S>,
    key_fn: fn(&Request) -> Option<&Key>,
    request_fn: fn(&Key) -> Request,
    merge_fn: fn(Request, S::Response) -> S::Response,
    #[allow(clippy::type_complexity)]
    wait_map: Arc<Mutex<HashMap<Key, Sender<Result<S::Response, String>>>>>,
}

impl<S, Request, Key> DeduplicationService<S, Request, Key>
where
    Request: Send + 'static,
    Key: Clone + Eq + Hash + Send + 'static,
    S: Service<Request> + Send + Clone,
    <S as Service<Request>>::Response: Clone + Send,
    <S as Service<Request>>::Error: Into<BoxError> + Send + Sync,
    <S as Service<Request>>::Future: Send,
{
    fn new(
        service: S,
        key_fn: fn(&Request) -> Option<&Key>,
        request_fn: fn(&Key) -> Request,
        merge_fn: fn(Request, S::Response) -> S::Response,
    ) -> Self {
        DeduplicationService {
            service,
            ready_service: None,
            key_fn,
            request_fn,
            merge_fn,
            wait_map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[allow(clippy::type_complexity)]
    async fn dedup(
        mut service: S,
        wait_map: Arc<Mutex<HashMap<Key, Sender<Result<S::Response, String>>>>>,
        key: Key,
        request_fn: fn(&Key) -> Request,
    ) -> Result<S::Response, BoxError> {
        loop {
            let mut locked_wait_map = wait_map.lock().await;
            let waiter = locked_wait_map.get_mut(&key);
            match waiter {
                Some(waiter) => {
                    // Register interest in key
                    let mut receiver = waiter.subscribe();
                    drop(locked_wait_map);

                    match receiver.recv().await {
                        Ok(value) => return value.map_err(Into::into),
                        // there was an issue with the broadcast channel, retry fetching
                        Err(_) => continue,
                    }
                }
                None => {
                    let (tx, _rx) = broadcast::channel(1);
                    locked_wait_map.insert(key.clone(), tx.clone());
                    drop(locked_wait_map);

                    let downstream_request = (request_fn)(&key);
                    let res = service.call(downstream_request).await;

                    {
                        let mut locked_wait_map = wait_map.lock().await;
                        locked_wait_map.remove(&key);
                    }

                    // Let our waiters know
                    // We map the error into a string because we can't guarantee the error type is clone.
                    let value = res.map_err(|e| Into::<BoxError>::into(e).to_string());

                    // Our use case is very specific, so we are sure that
                    // we won't get any errors here.

                    // Note that previous implementation notified in waiters in a separate thread.
                    // However the logic in the broadcast channel is such that a single mutex
                    // is updated and the waiting threads are notified to wake.
                    // It introduces a reasonable performance hit to use the extra thread just to update
                    // the mutex and woke the waiting threads.
                    tx.send(value.clone())
                        .map_err(|_| ())
                        .expect("there is always at least one receiver alive, the _rx guard; qed");

                    return value.map_err(Into::into);
                }
            }
        }
    }
}

impl<S, Request, Key> Service<Request> for DeduplicationService<S, Request, Key>
where
    Request: Send + 'static,
    Key: Clone + Eq + Hash + Send + 'static,
    S: Service<Request> + Send + Clone + 'static,
    <S as Service<Request>>::Response: Clone + Send + 'static,
    <S as Service<Request>>::Error: Into<BoxError> + Send + Sync + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.ready_service
            .get_or_insert_with(|| self.service.clone())
            .poll_ready(cx)
            .map_err(Into::into)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let mut service = self.ready_service.take().unwrap();
        let key = (self.key_fn)(&request);
        match key {
            Some(key) => {
                let request_fn = self.request_fn;
                let merge_fn = self.merge_fn;
                let wait_map = self.wait_map.clone();
                let key = key.clone();
                Box::pin(async move {
                    Self::dedup(service, wait_map, key, request_fn)
                        .await
                        .map(|response| (merge_fn)(request, response))
                })
            }
            None => service.call(request).map_err(Into::into).boxed(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{mock_service, ServiceBuilderExt};
    use mockall::predicate::eq;
    use std::time::Duration;
    use std::vec::Vec;
    use test_log::test;
    use tower::filter::AsyncPredicate;
    use tower::ServiceBuilder;
    use tower::ServiceExt;

    #[derive(Clone, Eq, PartialEq, Debug)]
    pub struct A {
        key: String,
        dummy: String,
    }

    #[derive(Clone, Eq, PartialEq, Debug)]
    pub struct B {
        key: String,
        value: String,
    }

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

    mock_service!(AB, A, B);

    #[test(tokio::test)]
    async fn it_should_not_dedupe_non_cachable() {
        let mut mock_service = MockABService::new();

        mock_service
            .expect_call()
            .times(1)
            .with(eq(A {
                key: "Not cacheable".into(),
                dummy: "Needed".into(),
            }))
            .returning(move |a| {
                Ok(B {
                    key: a.key,
                    value: "there".into(),
                })
            });

        let mut service = create_service(mock_service);

        service
            .ready()
            .await
            .unwrap()
            .call(A {
                key: "Not cacheable".into(),
                dummy: "Needed".into(),
            })
            .await
            .unwrap();
    }

    #[test(tokio::test)]
    async fn it_should_dedupe_in_flight_calls() {
        let mut mock_service = MockABService::new();

        mock_service
            .expect_call()
            .times(1)
            .with(eq(A {
                key: "CacheableA".into(),
                dummy: "".into(),
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
                key: "CacheableA".into(),
                dummy: "Not needed".into(),
            }));
        }

        let results = futures::future::join_all(tasks).await;
        for result in results {
            assert_eq!(
                result,
                Ok(B {
                    key: "CacheableA".into(),
                    value: "there".into()
                })
            );
        }
    }

    fn create_service(
        mock_service: MockABService,
    ) -> impl Service<A, Response = B, Error = String> {
        ServiceBuilder::new()
            .dedup(
                |request: &A| {
                    if request.key.starts_with("Cacheable") {
                        Some(&request.key)
                    } else {
                        None
                    }
                },
                |key: &String| A {
                    key: key.clone(),
                    dummy: "".into(),
                },
                |request: A, response: B| B {
                    key: request.key,
                    value: response.value,
                },
            )
            .filter_async(Slow::default())
            .service(mock_service.build())
            .map_err(|e: BoxError| e.to_string())
    }
}
