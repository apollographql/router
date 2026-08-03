use std::ops::ControlFlow;
use std::sync::Arc;

use futures::future::BoxFuture;
use tower::BoxError;
use tower::Layer;
use tower::Service;

use super::runtime::Runtime;
use crate::layers::async_checkpoint::AsyncCheckpointService;
use crate::services::connector::request_service;
use crate::services::subgraph;
use crate::services::supergraph;

type SupergraphFuture =
    BoxFuture<'static, Result<ControlFlow<supergraph::Response, supergraph::Request>, BoxError>>;

#[derive(Clone)]
pub(super) struct WasmSupergraphLayer {
    runtime: Arc<Runtime>,
}

impl WasmSupergraphLayer {
    pub(super) fn new(runtime: Arc<Runtime>) -> Self {
        Self { runtime }
    }
}

impl<S> Layer<S> for WasmSupergraphLayer
where
    S: Service<supergraph::Request, Response = supergraph::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = AsyncCheckpointService<S, SupergraphFuture, supergraph::Request>;

    fn layer(&self, inner: S) -> Self::Service {
        let runtime = self.runtime.clone();
        AsyncCheckpointService::new(
            move |request| -> SupergraphFuture {
                let runtime = runtime.clone();
                Box::pin(async move { runtime.process_supergraph_request(request).await })
            },
            inner,
        )
    }
}

type SubgraphFuture =
    BoxFuture<'static, Result<ControlFlow<subgraph::Response, subgraph::Request>, BoxError>>;

#[derive(Clone)]
pub(super) struct WasmSubgraphLayer {
    runtime: Arc<Runtime>,
    service_name: Arc<str>,
}

type ConnectorFuture = BoxFuture<
    'static,
    Result<ControlFlow<request_service::Response, request_service::Request>, BoxError>,
>;

#[derive(Clone)]
pub(super) struct WasmConnectorLayer {
    runtime: Arc<Runtime>,
}

impl WasmConnectorLayer {
    pub(super) fn new(runtime: Arc<Runtime>) -> Self {
        Self { runtime }
    }
}

impl<S> Layer<S> for WasmConnectorLayer
where
    S: Service<request_service::Request, Response = request_service::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = AsyncCheckpointService<S, ConnectorFuture, request_service::Request>;

    fn layer(&self, inner: S) -> Self::Service {
        let runtime = self.runtime.clone();
        AsyncCheckpointService::new(
            move |request| -> ConnectorFuture {
                let runtime = runtime.clone();
                Box::pin(async move { runtime.process_connector_request(request).await })
            },
            inner,
        )
    }
}

impl WasmSubgraphLayer {
    pub(super) fn new(runtime: Arc<Runtime>, service_name: Arc<str>) -> Self {
        Self {
            runtime,
            service_name,
        }
    }
}

impl<S> Layer<S> for WasmSubgraphLayer
where
    S: Service<subgraph::Request, Response = subgraph::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = AsyncCheckpointService<S, SubgraphFuture, subgraph::Request>;

    fn layer(&self, inner: S) -> Self::Service {
        let runtime = self.runtime.clone();
        let service_name = self.service_name.clone();
        AsyncCheckpointService::new(
            move |request| -> SubgraphFuture {
                let runtime = runtime.clone();
                let service_name = service_name.clone();
                Box::pin(async move {
                    runtime
                        .process_subgraph_request(request, &service_name)
                        .await
                })
            },
            inner,
        )
    }
}

#[cfg(test)]
mod tests {
    use tower::ServiceExt;

    use super::*;
    use crate::plugins::wasm::config::WasmConfig;

    #[tokio::test]
    async fn supergraph_layer_forwards_continued_requests() {
        let (inner, mut handle) =
            tower_test::mock::pair::<supergraph::Request, supergraph::Response>();
        let driver = tokio::spawn(async move {
            let (request, responder) = handle
                .next_request()
                .await
                .expect("continued request should reach the inner service");
            responder.send_response(
                supergraph::Response::fake_builder()
                    .context(request.context)
                    .build()
                    .unwrap(),
            );
        });
        let runtime = Arc::new(Runtime::new(WasmConfig::default()).unwrap());
        let service = WasmSupergraphLayer::new(runtime).layer(inner);

        service
            .oneshot(supergraph::Request::fake_builder().build().unwrap())
            .await
            .unwrap();

        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn subgraph_layer_forwards_continued_requests() {
        let (inner, mut handle) = tower_test::mock::pair::<subgraph::Request, subgraph::Response>();
        let driver = tokio::spawn(async move {
            let (request, responder) = handle
                .next_request()
                .await
                .expect("continued request should reach the inner service");
            responder.send_response(
                subgraph::Response::fake_builder()
                    .context(request.context)
                    .build(),
            );
        });
        let runtime = Arc::new(Runtime::new(WasmConfig::default()).unwrap());
        let service = WasmSubgraphLayer::new(runtime, Arc::from("products")).layer(inner);

        service
            .oneshot(subgraph::Request::fake_builder().build())
            .await
            .unwrap();

        crate::plugin::test::await_mock_driver(driver).await;
    }
}
