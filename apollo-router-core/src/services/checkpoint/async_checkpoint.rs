use super::Step;
use futures::future::BoxFuture;
use std::sync::Arc;
use tower::{
    buffer::Buffer, util::BoxCloneService, BoxError, Layer, Service, ServiceBuilder, ServiceExt,
};

#[allow(clippy::type_complexity)]
pub struct AsyncCheckpointLayer<S, Request>
where
    S: Service<Request> + Send + 'static,
    Request: Send + 'static,
    S::Future: Send,
    S::Response: Send + 'static,
    <S as Service<Request>>::Error: Into<BoxError> + Send + Sync + 'static,
{
    checkpoint_fn: Arc<
        dyn Fn(
                Request,
            ) -> BoxFuture<
                'static,
                Result<Step<Request, <S as Service<Request>>::Response>, BoxError>,
            > + Send
            + Sync
            + 'static,
    >,
}

#[allow(clippy::type_complexity)]
impl<S, Request> AsyncCheckpointLayer<S, Request>
where
    S: Service<Request> + Send + 'static,
    Request: Send + 'static,
    S::Future: Send,
    S::Response: Send + 'static,
    <S as Service<Request>>::Error: Into<BoxError> + Send + Sync + 'static,
{
    /// Create an `AsyncCheckpointLayer` from a function that takes a Service Request and returns a `Step`
    pub fn new(
        checkpoint_fn: impl Fn(
                Request,
            ) -> BoxFuture<
                'static,
                Result<Step<Request, <S as Service<Request>>::Response>, BoxError>,
            > + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            checkpoint_fn: Arc::new(checkpoint_fn),
        }
    }
}

impl<S, Request> Layer<S> for AsyncCheckpointLayer<S, Request>
where
    S: Service<Request> + Send + 'static,
    <S as Service<Request>>::Future: Send,
    Request: Send + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Error: Into<BoxError> + Send + Sync + 'static,
{
    type Service = AsyncCheckpointService<
        BoxCloneService<Request, <S as Service<Request>>::Response, BoxError>,
        Request,
    >;

    fn layer(&self, service: S) -> Self::Service {
        let inner = Buffer::new(service, 20_000);
        AsyncCheckpointService {
            checkpoint_fn: Arc::clone(&self.checkpoint_fn),
            inner: ServiceBuilder::new().service(inner).boxed_clone(),
        }
    }
}

#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub struct AsyncCheckpointService<S, Request>
where
    Request: Send + 'static,
    S: Service<Request> + Send + 'static,
    <S as Service<Request>>::Error: Into<BoxError> + Send + Sync + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    inner: BoxCloneService<Request, <S as Service<Request>>::Response, BoxError>,
    checkpoint_fn: Arc<
        dyn Fn(
                Request,
            ) -> BoxFuture<
                'static,
                Result<Step<Request, <S as Service<Request>>::Response>, BoxError>,
            > + Send
            + Sync
            + 'static,
    >,
}

#[allow(clippy::type_complexity)]
impl<S, Request> AsyncCheckpointService<S, Request>
where
    Request: Send + 'static,
    S: Service<Request> + Send + 'static,
    <S as Service<Request>>::Error: Into<BoxError> + Send + Sync + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    /// Create an `AsyncCheckpointLayer` from a function that takes a Service Request and returns a `Step`
    pub fn new(
        checkpoint_fn: impl Fn(
                Request,
            ) -> BoxFuture<
                'static,
                Result<Step<Request, <S as Service<Request>>::Response>, BoxError>,
            > + Send
            + Sync
            + 'static,
        service: S,
    ) -> Self {
        let inner = Buffer::new(service, 20_000);
        Self {
            checkpoint_fn: Arc::new(checkpoint_fn),
            inner: ServiceBuilder::new().service(inner).boxed_clone(),
        }
    }
}

impl<S, Request> Service<Request> for AsyncCheckpointService<S, Request>
where
    Request: Send + 'static,
    S: Service<Request> + Send + 'static,
    <S as Service<Request>>::Error: Into<BoxError> + Send + Sync + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    type Response = <S as Service<Request>>::Response;

    type Error = BoxError;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let checkpoint_fn = Arc::clone(&self.checkpoint_fn);
        let inner = self.inner.clone();
        Box::pin(async move {
            match (checkpoint_fn)(req).await {
                Ok(Step::Return(response)) => Ok(response),
                Ok(Step::Continue(request)) => inner.oneshot(request).await,
                Err(error) => Err(BoxError::from(error)),
            }
        })
    }
}

#[cfg(test)]
mod async_checkpoint_tests {
    use super::*;
    use crate::{
        plugin_utils::{ExecutionRequest, ExecutionResponse, MockExecutionService},
        ServiceBuilderExt,
    };
    use tower::{BoxError, Layer, ServiceBuilder, ServiceExt};

    #[tokio::test]
    async fn test_service_builder() {
        let expected_label = "from_mock_service";

        let mut execution_service = MockExecutionService::new();

        execution_service
            .expect_call()
            .times(1)
            .returning(move |_req: crate::ExecutionRequest| {
                Ok(ExecutionResponse::builder()
                    .label(expected_label.to_string())
                    .build()
                    .into())
            });

        let service = execution_service.build();

        let service_stack = ServiceBuilder::new()
            .async_checkpoint(|req: crate::ExecutionRequest| {
                Box::pin(async { Ok(Step::Continue(req)) })
            })
            .service(service);

        let request = ExecutionRequest::builder().build().into();

        let actual_label = service_stack
            .oneshot(request)
            .await
            .unwrap()
            .response
            .into_body()
            .label
            .unwrap();

        assert_eq!(actual_label, expected_label)
    }

    #[tokio::test]
    async fn test_continue() {
        let expected_label = "from_mock_service";
        let mut router_service = MockExecutionService::new();

        router_service
            .expect_call()
            .times(1)
            .returning(move |_req| {
                Ok(ExecutionResponse::builder()
                    .label(expected_label.to_string())
                    .build()
                    .into())
            });

        let service = router_service.build();

        let service_stack =
            AsyncCheckpointLayer::new(|req| Box::pin(async { Ok(Step::Continue(req)) }))
                .layer(service);

        let request = ExecutionRequest::builder().build().into();

        let actual_label = service_stack
            .oneshot(request)
            .await
            .unwrap()
            .response
            .into_body()
            .label
            .unwrap();

        assert_eq!(actual_label, expected_label)
    }

    #[tokio::test]
    async fn test_return() {
        let expected_label = "returned_before_mock_service";
        let router_service = MockExecutionService::new();

        let service = router_service.build();

        let service_stack = AsyncCheckpointLayer::new(|_req| {
            Box::pin(async {
                Ok(Step::Return(
                    ExecutionResponse::builder()
                        .label("returned_before_mock_service".to_string())
                        .build()
                        .into(),
                ))
            })
        })
        .layer(service);

        let request = ExecutionRequest::builder().build().into();

        let actual_label = service_stack
            .oneshot(request)
            .await
            .unwrap()
            .response
            .into_body()
            .label
            .unwrap();

        assert_eq!(actual_label, expected_label)
    }

    #[tokio::test]
    async fn test_error() {
        let expected_error = "checkpoint_error";
        let router_service = MockExecutionService::new();

        let service = router_service.build();

        let service_stack = AsyncCheckpointLayer::new(move |_req| {
            Box::pin(async move { Err(BoxError::from(expected_error)) })
        })
        .layer(service);

        let request = ExecutionRequest::builder().build().into();

        let actual_error = service_stack
            .oneshot(request)
            .await
            .map(|_| unreachable!())
            .unwrap_err()
            .to_string();

        assert_eq!(actual_error, expected_error)
    }
}
