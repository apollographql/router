//! De-duplicate connector requests in flight. Implemented as a tower Layer.
//!
//! See [`Layer`] and [`tower::Service`] for more details.

use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;

use futures::future::BoxFuture;
use futures::lock::Mutex;
use tokio::sync::broadcast::Sender;
use tokio::sync::broadcast::{self};
use tokio::sync::oneshot;
use tower::BoxError;
use tower::Layer;

use crate::batching::BatchQuery;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::query_planner::fetch::OperationKind;
use crate::services::connector::request_service::Response as ConnectorResponse;
use crate::services::connector::request_service::{Request as ConnectorRequest, TransportRequest};

#[derive(Default)]
pub(crate) struct ConnectorDeduplicationLayer;

impl<S> Layer<S> for ConnectorDeduplicationLayer
where
    S: tower::Service<ConnectorRequest, Response = ConnectorResponse, Error = BoxError> + Clone,
{
    type Service = ConnectorDeduplicationService<S>;

    fn layer(&self, service: S) -> Self::Service {
        ConnectorDeduplicationService::new(service)
    }
}

type CacheKey = (String, Arc<CacheKeyMetadata>);

type WaitMap = Arc<Mutex<HashMap<CacheKey, Sender<Result<CloneConnectorResponse, String>>>>>;

struct CloneConnectorResponse(ConnectorResponse);

impl Clone for CloneConnectorResponse {
    fn clone(&self) -> Self {
        Self(ConnectorResponse {
            context: self.0.context.clone(),
            connector: self.0.connector.clone(),
            transport_result: self.0.transport_result.clone(),
            mapped_response: self.0.mapped_response.clone(),
        })
    }
}

#[derive(Clone)]
pub(crate) struct ConnectorDeduplicationService<S: Clone> {
    service: S,
    wait_map: WaitMap,
}

impl<S> ConnectorDeduplicationService<S>
where
    S: tower::Service<ConnectorRequest, Response = ConnectorResponse, Error = BoxError> + Clone,
{
    fn new(service: S) -> Self {
        ConnectorDeduplicationService {
            service,
            wait_map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn dedup(
        mut service: S,
        wait_map: WaitMap,
        request: ConnectorRequest,
    ) -> Result<ConnectorResponse, BoxError> {
        // Check if the request is part of a batch. If it is, completely bypass dedup since it
        // will break any request batches which this request is part of.
        // This check is what enables Batching and Dedup to work together, so be very careful
        // before making any changes to it.
        if request
            .context
            .extensions()
            .with_lock(|lock| lock.contains_key::<BatchQuery>())
        {
            return service.call(request).await;
        }
        loop {
            let mut locked_wait_map = wait_map.lock().await;
            let authorization_cache_key = request.authorization.clone();
            let TransportRequest::Http(ref http_request) = request.transport_request;
            let cache_key = ((&http_request.to_sha256()).into(), authorization_cache_key);

            match locked_wait_map.get_mut(&cache_key) {
                Some(waiter) => {
                    // Register interest in key
                    let mut receiver = waiter.subscribe();
                    drop(locked_wait_map);

                    match receiver.recv().await {
                        Ok(value) => {
                            return value
                                .map(|response| ConnectorResponse {
                                    context: request.context,
                                    connector: request.connector,
                                    transport_result: response.0.transport_result,
                                    mapped_response: response.0.mapped_response,
                                })
                                .map_err(|e| e.into());
                        }
                        // there was an issue with the broadcast channel, retry fetching
                        Err(_) => continue,
                    }
                }
                None => {
                    let (tx, _rx) = broadcast::channel(1);

                    locked_wait_map.insert(cache_key, tx.clone());
                    drop(locked_wait_map);

                    let context = request.context.clone();
                    let connector = request.connector.clone();
                    let authorization_cache_key = request.authorization.clone();
                    let TransportRequest::Http(ref http_request) = request.transport_request;
                    let cache_key = ((&http_request.to_sha256()).into(), authorization_cache_key);
                    let res = {
                        // when _drop_signal is dropped, either by getting out of the block, returning
                        // the error from ready_oneshot or by cancellation, the drop_sentinel future will
                        // return with Err(), then we remove the entry from the wait map
                        let (_drop_signal, drop_sentinel) = oneshot::channel::<()>();
                        tokio::task::spawn(async move {
                            let _ = drop_sentinel.await;
                            let mut locked_wait_map = wait_map.lock().await;
                            locked_wait_map.remove(&cache_key);
                        });

                        service.call(request).await.map(CloneConnectorResponse)
                    };

                    // Let our waiters know

                    // Clippy is wrong, the suggestion adds a useless clone of the error
                    #[allow(clippy::useless_asref)]
                    let broadcast_value = res
                        .as_ref()
                        .map(|response| response.clone())
                        .map_err(|e| e.to_string());

                    // We may get errors here, for instance if a task is cancelled,
                    // so just ignore the result of send
                    let _ = tokio::task::spawn_blocking(move || {
                        tx.send(broadcast_value)
                    }).await
                    .expect("can only fail if the task is aborted or if the internal code panics, neither is possible here; qed");

                    return res.map(|response| ConnectorResponse {
                        context,
                        connector,
                        transport_result: response.0.transport_result,
                        mapped_response: response.0.mapped_response,
                    });
                }
            }
        }
    }
}

impl<S> tower::Service<ConnectorRequest> for ConnectorDeduplicationService<S>
where
    S: tower::Service<ConnectorRequest, Response = ConnectorResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    <S as tower::Service<ConnectorRequest>>::Future: Send + 'static,
{
    type Response = ConnectorResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, request: ConnectorRequest) -> Self::Future {
        let service = self.service.clone();
        let mut inner = std::mem::replace(&mut self.service, service);

        if request.operation_kind == OperationKind::Query {
            let wait_map = self.wait_map.clone();

            Box::pin(async move { Self::dedup(inner, wait_map, request).await })
        } else {
            Box::pin(async move { inner.call(request).await })
        }
    }
}
