use crate::{http_compat, Request, SubgraphRequest, SubgraphResponse};
use futures::{future::BoxFuture, lock::Mutex};
use std::{collections::HashMap, sync::Arc, task::Poll};
use tokio::sync::broadcast::{self, Sender};
use tower::{BoxError, Layer, ServiceExt};

pub struct QueryDeduplicationLayer;

impl<S> Layer<S> for QueryDeduplicationLayer
where
    S: tower::Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError> + Clone,
{
    type Service = QueryDeduplicationService<S>;

    fn layer(&self, service: S) -> Self::Service {
        QueryDeduplicationService::new(service)
    }
}

pub struct QueryDeduplicationService<S> {
    service: S,
    wait_map: Arc<
        Mutex<HashMap<http_compat::Request<Request>, Sender<Result<SubgraphResponse, String>>>>,
    >,
}

impl<S> QueryDeduplicationService<S>
where
    S: tower::Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError> + Clone,
{
    fn new(service: S) -> Self {
        QueryDeduplicationService {
            service,
            wait_map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn dedup(
        service: S,
        wait_map: Arc<
            Mutex<HashMap<http_compat::Request<Request>, Sender<Result<SubgraphResponse, String>>>>,
        >,
        request: SubgraphRequest,
    ) -> Result<SubgraphResponse, BoxError> {
        loop {
            let mut locked_wait_map = wait_map.lock().await;
            match locked_wait_map.get_mut(&request.http_request) {
                Some(waiter) => {
                    // Register interest in key
                    let mut receiver = waiter.subscribe();
                    drop(locked_wait_map);

                    match receiver.recv().await {
                        Ok(value) => {
                            return value
                                .map(|response| SubgraphResponse {
                                    response: response.response,
                                    context: request.context,
                                })
                                .map_err(|e| e.into())
                        }
                        // there was an issue with the broadcast channel, retry fetching
                        Err(_) => continue,
                    }
                }
                None => {
                    let (tx, _rx) = broadcast::channel(1);
                    locked_wait_map.insert(request.http_request.clone(), tx.clone());
                    drop(locked_wait_map);

                    let context = request.context.clone();
                    let http_request = request.http_request.clone();
                    let res = service.ready_oneshot().await?.call(request).await;

                    {
                        let mut locked_wait_map = wait_map.lock().await;
                        locked_wait_map.remove(&http_request);
                    }

                    // Let our waiters know
                    let broadcast_value = res
                        .as_ref()
                        .map(|response| response.clone())
                        .map_err(|e| e.to_string());

                    // Our use case is very specific, so we are sure that
                    // we won't get any errors here.
                    tokio::task::spawn_blocking(move || {
                        tx.send(broadcast_value)
                            .expect("there is always at least one receiver alive, the _rx guard; qed")
                    }).await
                    .expect("can only fail if the task is aborted or if the internal code panics, neither is possible here; qed");

                    return res.map(|response| SubgraphResponse {
                        response: response.response,
                        context,
                    });
                }
            }
        }
    }
}

impl<S> tower::Service<SubgraphRequest> for QueryDeduplicationService<S>
where
    S: tower::Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    <S as tower::Service<SubgraphRequest>>::Future: Send + 'static,
{
    type Response = SubgraphResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, request: SubgraphRequest) -> Self::Future {
        let service = self.service.clone();
        let wait_map = self.wait_map.clone();

        Box::pin(async move { Self::dedup(service, wait_map, request).await })
    }
}
