//! Asynchronous Checkpoint
//!
//! Provides a general mechanism for controlling the flow of a request. Useful in any situation
//! where the caller wishes to provide control flow for a request.
//!
//! If the evaluated closure succeeds then the request is passed onto the next service in the
//! chain of responsibilities. If it fails, then the control flow is broken and a response is passed
//! back to the invoking service.
//!
//! See [`Layer`] and [`Service`] for more details.

use std::marker::PhantomData;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use futures::future::BoxFuture;
use tower::BoxError;
use tower::Layer;
use tower::Service;

/// [`Layer`] for Asynchronous Checkpoints. See [`ServiceBuilderExt::checkpoint_async()`](crate::layers::ServiceBuilderExt::checkpoint_async()).
#[allow(clippy::type_complexity)]
pub struct AsyncCheckpointLayer<S, Fut, Request>
where
    S: Service<Request, Error = BoxError> + Clone + Send + 'static,
    Fut: Future<Output = Result<ControlFlow<<S as Service<Request>>::Response, Request>, BoxError>>,
{
    checkpoint_fn: Arc<Pin<Box<dyn Fn(Request) -> Fut + Send + Sync + 'static>>>,
    phantom: PhantomData<S>, // We use PhantomData because the compiler can't detect that S is used in the Future.
}

impl<S, Fut, Request> AsyncCheckpointLayer<S, Fut, Request>
where
    S: Service<Request, Error = BoxError> + Clone + Send + 'static,
    Fut: Future<Output = Result<ControlFlow<<S as Service<Request>>::Response, Request>, BoxError>>,
{
    /// Create an `AsyncCheckpointLayer` from a function that takes a Service Request and returns a `ControlFlow`
    pub fn new<F>(checkpoint_fn: F) -> Self
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
    {
        Self {
            checkpoint_fn: Arc::new(Box::pin(checkpoint_fn)),
            phantom: PhantomData,
        }
    }
}

impl<S, Fut, Request> Layer<S> for AsyncCheckpointLayer<S, Fut, Request>
where
    S: Service<Request, Error = BoxError> + Clone + Send + 'static,
    <S as Service<Request>>::Future: Send,
    Request: Send + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    Fut: Future<Output = Result<ControlFlow<<S as Service<Request>>::Response, Request>, BoxError>>,
{
    type Service = AsyncCheckpointService<S, Fut, Request>;

    fn layer(&self, service: S) -> Self::Service {
        AsyncCheckpointService {
            checkpoint_fn: Arc::clone(&self.checkpoint_fn),
            service,
        }
    }
}

/// [`Service`] for Asynchronous Checkpoints. See [`ServiceBuilderExt::checkpoint_async()`](crate::layers::ServiceBuilderExt::checkpoint_async()).
#[allow(clippy::type_complexity)]
pub struct AsyncCheckpointService<S, Fut, Request>
where
    Request: Send + 'static,
    S: Service<Request, Error = BoxError> + Clone + Send + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
    Fut: Future<Output = Result<ControlFlow<<S as Service<Request>>::Response, Request>, BoxError>>,
{
    service: S,
    checkpoint_fn: Arc<Pin<Box<dyn Fn(Request) -> Fut + Send + Sync + 'static>>>,
}

impl<S, Fut, Request> Clone for AsyncCheckpointService<S, Fut, Request>
where
    Request: Send + 'static,
    S: Service<Request, Error = BoxError> + Clone + Send + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
    Fut: Future<Output = Result<ControlFlow<<S as Service<Request>>::Response, Request>, BoxError>>,
{
    fn clone(&self) -> Self {
        Self {
            service: self.service.clone(),
            checkpoint_fn: Arc::clone(&self.checkpoint_fn),
        }
    }
}

impl<S, Fut, Request> AsyncCheckpointService<S, Fut, Request>
where
    Request: Send + 'static,
    S: Service<Request, Error = BoxError> + Clone + Send + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
    Fut: Future<Output = Result<ControlFlow<<S as Service<Request>>::Response, Request>, BoxError>>,
{
    /// Create an `AsyncCheckpointLayer` from a function that takes a Service Request and returns a `ControlFlow`
    pub fn new<F>(checkpoint_fn: F, service: S) -> Self
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
    {
        Self {
            checkpoint_fn: Arc::new(Box::pin(checkpoint_fn)),
            service,
        }
    }
}

impl<S, Fut, Request> Service<Request> for AsyncCheckpointService<S, Fut, Request>
where
    Request: Send + 'static,
    S: Service<Request, Error = BoxError> + Clone + Send + 'static,
    <S as Service<Request>>::Response: Send + 'static,
    <S as Service<Request>>::Future: Send + 'static,
    Fut: Future<Output = Result<ControlFlow<<S as Service<Request>>::Response, Request>, BoxError>>
        + Send
        + 'static,
{
    type Response = <S as Service<Request>>::Response;

    type Error = BoxError;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let checkpoint_fn = Arc::clone(&self.checkpoint_fn);
        let service = self.service.clone();
        let mut inner = std::mem::replace(&mut self.service, service);

        Box::pin(async move {
            match (checkpoint_fn)(req).await {
                Ok(ControlFlow::Break(response)) => Ok(response),
                Ok(ControlFlow::Continue(request)) => inner.call(request).await,
                Err(error) => Err(error),
            }
        })
    }
}

#[cfg(test)]
mod async_checkpoint_tests {
    use tower::BoxError;
    use tower::Layer;
    use tower::ServiceBuilder;
    use tower::ServiceExt;

    use super::*;
    use crate::layers::ServiceBuilderExt;
    use crate::services::ExecutionRequest;
    use crate::services::ExecutionResponse;

    #[tokio::test]
    async fn test_service_builder() {
        let expected_label = "from_mock_service";
        let (mock, mut handle) = tower_test::mock::pair::<ExecutionRequest, ExecutionResponse>();

        // Spawn the handle driver because the async checkpoint future is lazy — the request
        // doesn't reach the inner service until the oneshot future is polled.
        let label = expected_label.to_string();
        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            responder.send_response(
                ExecutionResponse::fake_builder()
                    .label(label)
                    .context(req.context)
                    .build()
                    .unwrap(),
            );
        });

        let service_stack = ServiceBuilder::new()
            .checkpoint_async(|req: ExecutionRequest| async { Ok(ControlFlow::Continue(req)) })
            .service(mock);

        let actual_label = service_stack
            .oneshot(ExecutionRequest::fake_builder().build())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .label
            .unwrap();

        assert_eq!(actual_label, expected_label);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn test_continue() {
        let expected_label = "from_mock_service";
        let (mock, mut handle) = tower_test::mock::pair::<ExecutionRequest, ExecutionResponse>();

        let label = expected_label.to_string();
        let driver = tokio::spawn(async move {
            let (_req, responder) = handle.next_request().await.unwrap();
            responder.send_response(
                ExecutionResponse::fake_builder()
                    .label(label)
                    .build()
                    .unwrap(),
            );
        });

        let service_stack =
            AsyncCheckpointLayer::new(|req| async { Ok(ControlFlow::Continue(req)) }).layer(mock);

        let actual_label = service_stack
            .oneshot(ExecutionRequest::fake_builder().build())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .label
            .unwrap();

        assert_eq!(actual_label, expected_label);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    async fn test_return() {
        let (mock, handle) = tower_test::mock::pair::<ExecutionRequest, ExecutionResponse>();

        let service_stack = AsyncCheckpointLayer::new(|_req| async {
            Ok(ControlFlow::Break(
                ExecutionResponse::fake_builder()
                    .label("returned_before_mock_service".to_string())
                    .build()
                    .unwrap(),
            ))
        })
        .layer(mock);

        let actual_label = service_stack
            .oneshot(ExecutionRequest::fake_builder().build())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .label
            .unwrap();

        assert_eq!(actual_label, "returned_before_mock_service");
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test]
    async fn test_error() {
        let expected_error = "checkpoint_error";
        let (mock, handle) = tower_test::mock::pair::<ExecutionRequest, ExecutionResponse>();

        let service_stack =
            AsyncCheckpointLayer::new(
                move |_req| async move { Err(BoxError::from(expected_error)) },
            )
            .layer(mock);

        let actual_error = service_stack
            .oneshot(ExecutionRequest::fake_builder().build())
            .await
            .map(|_| unreachable!())
            .unwrap_err()
            .to_string();

        assert_eq!(actual_error, expected_error);
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test]
    async fn test_service_builder_oneshot() {
        let expected_label = "from_mock_service";
        let (mock, mut handle) = tower_test::mock::pair::<ExecutionRequest, ExecutionResponse>();

        let label = expected_label.to_string();
        let driver = tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            responder.send_response(
                ExecutionResponse::fake_builder()
                    .label(label)
                    .context(req.context)
                    .build()
                    .unwrap(),
            );
        });

        let service_stack = ServiceBuilder::new()
            .checkpoint_async(|req: ExecutionRequest| async { Ok(ControlFlow::Continue(req)) })
            .service(mock);

        let actual_label = service_stack
            .oneshot(ExecutionRequest::fake_builder().build())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .label
            .unwrap();

        assert_eq!(actual_label, expected_label);
        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test]
    #[should_panic]
    async fn test_service_builder_buffered_oneshot() {
        // service_fn responds immediately (no handle to drive), which lets the first call
        // complete so the second call can trigger the expected panic.
        let label = "from_mock_service".to_string();
        let svc = tower::service_fn(move |req: ExecutionRequest| {
            let label = label.clone();
            async move {
                Ok::<ExecutionResponse, BoxError>(
                    ExecutionResponse::fake_builder()
                        .label(label)
                        .context(req.context)
                        .build()
                        .unwrap(),
                )
            }
        });

        let mut service_stack = ServiceBuilder::new()
            .checkpoint_async(|req: ExecutionRequest| async { Ok(ControlFlow::Continue(req)) })
            .buffered()
            .service(svc);

        let _ = service_stack
            .call(ExecutionRequest::fake_builder().build())
            .await
            .unwrap();
        // Trying to use the service again should cause a panic
        let _ = service_stack
            .call(ExecutionRequest::fake_builder().build())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_double_ready_doesnt_panic() {
        let (mock, handle) = tower_test::mock::pair::<ExecutionRequest, ExecutionResponse>();

        let mut service_stack = AsyncCheckpointLayer::new(|_req| async {
            Ok(ControlFlow::Break(
                ExecutionResponse::fake_builder()
                    .label("returned_before_mock_service".to_string())
                    .build()
                    .unwrap(),
            ))
        })
        .layer(mock);

        service_stack.ready().await.unwrap();
        service_stack
            .call(ExecutionRequest::fake_builder().build())
            .await
            .unwrap();

        assert!(service_stack.ready().await.is_ok());
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test]
    async fn test_double_call_doesnt_panic() {
        let (mock, handle) = tower_test::mock::pair::<ExecutionRequest, ExecutionResponse>();

        let mut service_stack = AsyncCheckpointLayer::new(|_req| async {
            Ok(ControlFlow::Break(
                ExecutionResponse::fake_builder()
                    .label("returned_before_mock_service".to_string())
                    .build()
                    .unwrap(),
            ))
        })
        .layer(mock);

        service_stack.ready().await.unwrap();
        service_stack
            .call(ExecutionRequest::fake_builder().build())
            .await
            .unwrap();

        service_stack.ready().await.unwrap();
        assert!(
            service_stack
                .call(ExecutionRequest::fake_builder().build())
                .await
                .is_ok()
        );
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }
}
