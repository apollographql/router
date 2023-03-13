//! De-duplicate subgraph requests in flight. Implemented as a tower Layer.
//!
//! See [`Layer`] and [`tower::Service`] for more details.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::Poll;

use futures::future::BoxFuture;
use tokio::sync::oneshot;
use tokio::sync::OwnedRwLockWriteGuard;
use tokio::sync::RwLock;
use tower::BoxError;
use tower::Layer;
use tower::ServiceExt;
use tracing::Instrument;

use crate::graphql::Request;
use crate::http_ext;
use crate::query_planner::fetch::OperationKind;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;

#[derive(Default)]
pub(crate) struct QueryDeduplicationLayer;

impl<S> Layer<S> for QueryDeduplicationLayer
where
    S: tower::Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError> + Clone,
{
    type Service = QueryDeduplicationService<S>;

    fn layer(&self, service: S) -> Self::Service {
        QueryDeduplicationService::new(service)
    }
}

type WaitMap = Arc<
    Mutex<
        HashMap<
            http_ext::Request<Request>,
            Arc<RwLock<Option<Result<CloneSubgraphResponse, String>>>>,
        >,
    >,
>;

struct CloneSubgraphResponse(SubgraphResponse);

impl Clone for CloneSubgraphResponse {
    fn clone(&self) -> Self {
        Self(SubgraphResponse {
            response: http_ext::Response::from(&self.0.response).inner,
            context: self.0.context.clone(),
        })
    }
}

#[derive(Clone)]
pub(crate) struct QueryDeduplicationService<S: Clone> {
    service: S,
    wait_map: WaitMap,
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
        wait_map: WaitMap,
        request: SubgraphRequest,
    ) -> Result<SubgraphResponse, BoxError> {
        loop {
            match get_or_insert_wait_map(&wait_map, &request) {
                Err(receiver) => {
                    let r = receiver
                        .read()
                        .instrument(tracing::info_span!(
                            "traffic_shaping::dedup wait for receiver"
                        ))
                        .await;
                    return (*r)
                        .clone()
                        .unwrap()
                        .map(|response| {
                            SubgraphResponse::new_from_response(
                                response.0.response,
                                request.context,
                            )
                        })
                        .map_err(|e| e.into());
                }
                Ok(mut tx) => {
                    let context = request.context.clone();
                    let http_request = (&request.subgraph_request).into();
                    let res = {
                        // when _drop_signal is dropped, either by getting out of the block, returning
                        // the error from ready_oneshot or by cancellation, the drop_sentinel future will
                        // return with Err(), then we remove the entry from the wait map
                        let (_drop_signal, drop_sentinel) = oneshot::channel::<()>();
                        tokio::task::spawn(async move {
                            let _ = drop_sentinel.await;
                            match wait_map.lock() {
                                Ok(mut locked_wait_map) => {
                                    locked_wait_map.remove(&http_request);
                                }
                                Err(_e) => {}
                            };
                        });

                        service
                            .ready_oneshot()
                            .await?
                            .call(request)
                            .instrument(tracing::info_span!(
                                "traffic_shaping::dedup wait for service call"
                            ))
                            .await
                            .map(CloneSubgraphResponse)
                    };

                    tracing::info_span!("traffic_shaping::dedup broadcast").in_scope(|| {
                        // Let our waiters know
                        let broadcast_value = res
                            .as_ref()
                            .map(|response| response.clone())
                            .map_err(|e| e.to_string());

                        // We may get errors here, for instance if a task is cancelled,
                        // so just ignore the result of send
                        //let _ = tx.send(Some(broadcast_value));
                        *tx = Some(broadcast_value);
                    });

                    return res.map(|response| {
                        SubgraphResponse::new_from_response(response.0.response, context)
                    });
                }
            }
        }
    }
}

#[allow(clippy::type_complexity)]
fn get_or_insert_wait_map(
    wait_map: &WaitMap,
    request: &SubgraphRequest,
) -> Result<
    OwnedRwLockWriteGuard<Option<Result<CloneSubgraphResponse, String>>>,
    Arc<RwLock<Option<Result<CloneSubgraphResponse, String>>>>,
> {
    let mut locked_wait_map = match wait_map.lock() {
        Ok(guard) => guard,
        Err(_e) => panic!(),
    };
    match locked_wait_map.get_mut(&(&request.subgraph_request).into()) {
        Some(waiter) => {
            // Register interest in key
            let receiver = waiter.clone();
            drop(locked_wait_map);

            Err(receiver)
        }
        None => {
            let value = Arc::new(RwLock::new(None));
            let w = value.clone().try_write_owned().unwrap();

            locked_wait_map.insert((&request.subgraph_request).into(), value);
            drop(locked_wait_map);

            Ok(w)
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

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: SubgraphRequest) -> Self::Future {
        let service = self.service.clone();

        if request.operation_kind == OperationKind::Query {
            let wait_map = self.wait_map.clone();

            Box::pin(async move { Self::dedup(service, wait_map, request).await })
        } else {
            Box::pin(async move { service.oneshot(request).await })
        }
    }
}
