//! Asynchronous Checkpoint [`Layer`].
//!
//! Provides a general mechanism for controlling the flow of a request. Useful in any situation
//! where the caller wishes to provide control flow for a request.
//!
//! If the evaluated closure succeeds then the request is passed onto the next service in the
//! chain of responsibilities. If it fails, then the control flow is broken a response is passed
//! back to the invoking service.

use futures::future::BoxFuture;
use std::{ops::ControlFlow, sync::Arc};
use tower::{BoxError, Layer, Service, ServiceExt};

/// [`Layer`] for Asynchronous Checkpoints.
#[allow(clippy::type_complexity)]
pub struct AsyncCheckpointLayer<S, Request>
where
    S: Service<Request, Error = BoxError> + Clone + Send + 'static,
    Request: Send + 'static,
    S::Future: Send,
    S::Response: Send + 'static,
{
    checkpoint_fn: Arc<
        dyn Fn(
                Request,
            ) -> BoxFuture<
                'static,
                Result<ControlFlow<<S as Service<Request>>::Response, Request>, BoxError>,
            > + Send
            + Sync
            + 'static,
    >,
}

#[allow(clippy::type_complexity)]
impl<S, Request> AsyncCheckpointLayer<S, Request>
where
    S: Service<Request, Error = BoxError> + Clone + Send + 'static,
    Request: Send + 'static,
    S::Future: Send,
    S::Response: Send + 'static,
{
    /// Create an `AsyncCheckpointLayer` from a function that takes a Service Request and returns a `ControlFlow`
    pub fn new(
        checkpoint_fn: impl Fn(
                Request,
            ) -> BoxFuture<
                'static,
                Result<ControlFlow<<S as Service<Request>>::Response, Request>, BoxError>,
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
    S: Service<Request, Error = BoxError> + Clone + Send + 'static,
    <S as Service<Request>>::Future: Send,
    Request: Send + 'static,
    <S as Service<Request>>::Response: Send + 'static,
{
    type Service = AsyncCheckpointService<S, Request>;

    fn layer(&self, service: S) -> Self::Service {
        AsyncCheckpointService {
            checkpoint_fn: Arc::clone(&self.checkpoint_fn),
            inner: service,
        }
    }
}

/// [`Service`] for Asynchronous Checkpoints.
#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub struct AsyncCheckpointService<S, Request>
where
    Request: Send + 'static,
    S: Service<Request, Error = BoxError> + Clone + Send + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    inner: S,
    checkpoint_fn: Arc<
        dyn Fn(
                Request,
            ) -> BoxFuture<
                'static,
                Result<ControlFlow<<S as Service<Request>>::Response, Request>, BoxError>,
            > + Send
            + Sync
            + 'static,
    >,
}

#[allow(clippy::type_complexity)]
impl<S, Request> AsyncCheckpointService<S, Request>
where
    Request: Send + 'static,
    S: Service<Request, Error = BoxError> + Clone + Send + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    /// Create an `AsyncCheckpointLayer` from a function that takes a Service Request and returns a `ControlFlow`
    pub fn new(
        checkpoint_fn: impl Fn(
                Request,
            ) -> BoxFuture<
                'static,
                Result<ControlFlow<<S as Service<Request>>::Response, Request>, BoxError>,
            > + Send
            + Sync
            + 'static,
        service: S,
    ) -> Self {
        Self {
            checkpoint_fn: Arc::new(checkpoint_fn),
            inner: service,
        }
    }
}

impl<S, Request> Service<Request> for AsyncCheckpointService<S, Request>
where
    Request: Send + 'static,
    S: Service<Request, Error = BoxError> + Clone + Send + 'static,
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
                Ok(ControlFlow::Break(response)) => Ok(response),
                Ok(ControlFlow::Continue(request)) => inner.oneshot(request).await,
                Err(error) => Err(error),
            }
        })
    }
}

#[cfg(test)]
mod async_checkpoint_tests {
    use super::*;
    use crate::{
        plugin::utils::test::MockExecutionService, ExecutionRequest, ExecutionResponse,
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
                Ok(ExecutionResponse::fake_builder()
                    .label(expected_label.to_string())
                    .build())
            });

        let service = execution_service.build();

        let service_stack = ServiceBuilder::new()
            .async_checkpoint(|req: crate::ExecutionRequest| {
                Box::pin(async { Ok(ControlFlow::Continue(req)) })
            })
            .service(service);

        let request = ExecutionRequest::fake_builder().build();

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
                Ok(ExecutionResponse::fake_builder()
                    .label(expected_label.to_string())
                    .build())
            });

        let service = router_service.build();

        let service_stack =
            AsyncCheckpointLayer::new(|req| Box::pin(async { Ok(ControlFlow::Continue(req)) }))
                .layer(service);

        let request = ExecutionRequest::fake_builder().build();

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
                Ok(ControlFlow::Break(
                    ExecutionResponse::fake_builder()
                        .label("returned_before_mock_service".to_string())
                        .build(),
                ))
            })
        })
        .layer(service);

        let request = ExecutionRequest::fake_builder().build();

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

        let request = ExecutionRequest::fake_builder().build();

        let actual_error = service_stack
            .oneshot(request)
            .await
            .map(|_| unreachable!())
            .unwrap_err()
            .to_string();

        assert_eq!(actual_error, expected_error)
    }
}
