use futures::future::BoxFuture;
use std::{ops::ControlFlow, sync::Arc};
use tower::{BoxError, Layer, Service};

#[allow(clippy::type_complexity)]
pub struct CheckpointLayer<S, Request>
where
    S: Service<Request> + Send + 'static,
    Request: Send + 'static,
    S::Future: Send,
    S::Response: Send + 'static,
    S::Error: Send + 'static,
{
    checkpoint_fn: Arc<
        dyn Fn(
                Request,
            ) -> Result<
                ControlFlow<<S as Service<Request>>::Response, Request>,
                <S as Service<Request>>::Error,
            > + Send
            + Sync
            + 'static,
    >,
}

#[allow(clippy::type_complexity)]
impl<S, Request> CheckpointLayer<S, Request>
where
    S: Service<Request> + Send + 'static,
    Request: Send + 'static,
    S::Future: Send,
    S::Response: Send + 'static,
    <S as Service<Request>>::Error: Into<BoxError> + Send + 'static,
{
    /// Create a `CheckpointLayer` from a function that takes a Service Request and returns a `ControlFlow`
    pub fn new(
        checkpoint_fn: impl Fn(
                Request,
            ) -> Result<
                ControlFlow<<S as Service<Request>>::Response, Request>,
                <S as Service<Request>>::Error,
            > + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            checkpoint_fn: Arc::new(checkpoint_fn),
        }
    }
}

impl<S, Request> Layer<S> for CheckpointLayer<S, Request>
where
    S: Service<Request> + Send + 'static,
    <S as Service<Request>>::Future: Send,
    Request: Send + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Error: Into<BoxError> + Send + 'static,
{
    type Service = CheckpointService<S, Request>;

    fn layer(&self, service: S) -> Self::Service {
        CheckpointService {
            checkpoint_fn: Arc::clone(&self.checkpoint_fn),
            inner: service,
        }
    }
}

#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub struct CheckpointService<S, Request>
where
    Request: Send + 'static,
    S: Service<Request> + Send + 'static,
    <S as Service<Request>>::Error: Into<BoxError> + Send + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    inner: S,
    checkpoint_fn: Arc<
        dyn Fn(
                Request,
            ) -> Result<
                ControlFlow<<S as Service<Request>>::Response, Request>,
                <S as Service<Request>>::Error,
            > + Send
            + Sync
            + 'static,
    >,
}

#[allow(clippy::type_complexity)]
impl<S, Request> CheckpointService<S, Request>
where
    Request: Send + 'static,
    S: Service<Request> + Send + 'static,
    <S as Service<Request>>::Error: Into<BoxError> + Send + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
{
    /// Create a `CheckpointLayer` from a function that takes a Service Request and returns a `ControlFlow`
    pub fn new(
        checkpoint_fn: impl Fn(
                Request,
            ) -> Result<
                ControlFlow<<S as Service<Request>>::Response, Request>,
                <S as Service<Request>>::Error,
            > + Send
            + Sync
            + 'static,
        inner: S,
    ) -> Self {
        Self {
            checkpoint_fn: Arc::new(checkpoint_fn),
            inner,
        }
    }
}

impl<S, Request> Service<Request> for CheckpointService<S, Request>
where
    S: Service<Request>,
    S: Send + 'static,
    S::Future: Send,
    Request: Send + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Error: Into<BoxError> + Send + 'static,
{
    type Response = <S as Service<Request>>::Response;

    type Error = <S as Service<Request>>::Error;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        match (self.checkpoint_fn)(req) {
            Ok(ControlFlow::Break(response)) => Box::pin(async move { Ok(response) }),
            Ok(ControlFlow::Continue(request)) => Box::pin(self.inner.call(request)),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
}

#[cfg(test)]
mod checkpoint_tests {
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
            .checkpoint(|req: crate::ExecutionRequest| Ok(ControlFlow::Continue(req)))
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
            CheckpointLayer::new(|req| Ok(ControlFlow::Continue(req))).layer(service);

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

        let service_stack = CheckpointLayer::new(|_req| {
            Ok(ControlFlow::Break(
                ExecutionResponse::builder()
                    .label("returned_before_mock_service".to_string())
                    .build()
                    .into(),
            ))
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

        let service_stack =
            CheckpointLayer::new(move |_req| Err(BoxError::from(expected_error))).layer(service);

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
