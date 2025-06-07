use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;
use tower::{Layer, Service, ServiceExt};

/// Example service for documentation and testing purposes
///
/// This service simply passes requests through unchanged to demonstrate
/// tower test patterns without coupling to specific business logic.
#[derive(Clone)]
pub struct ExampleService<S> {
    inner: S,
}

impl<S> ExampleService<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S, Req> Service<Req> for ExampleService<S>
where
    S: Service<Req>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        self.inner.call(req)
    }
}

/// Example layer for documentation and testing purposes
///
/// This layer wraps services with the ExampleService to demonstrate
/// tower test patterns without coupling to specific business logic.
#[derive(Clone)]
pub struct ExampleLayer;

impl<S> Layer<S> for ExampleLayer {
    type Service = ExampleService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ExampleService::new(inner)
    }
}

/// Builder for testing tower layers with better error handling and panic detection
///
/// This builder provides a fluent API for configuring and running layer tests.
/// Unlike traditional tower tests, this builder provides:
/// - Automatic timeout protection to prevent hanging tests
/// - Panic detection in expectation handlers
/// - Clean separation of configuration and terminal methods
/// - No type annotation required for expectations!
///
/// # Example
///
/// ```rust,no_run
/// # use apollo_router_core::test_utils::tower_test::{TowerTest, ExampleLayer};
/// # use std::time::Duration;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let layer = ExampleLayer;
/// let request = http::Request::builder()
///     .uri("http://example.com")
///     .body("test body")
///     .unwrap();
///
/// let response = TowerTest::builder()
///     .layer(layer)
///     .timeout(Duration::from_secs(2))
///     .oneshot(request, |mut downstream| async move {
///         downstream.allow(1);
///         let (request, response) = downstream.next_request().await.expect("service must not fail");
///         // Set up expectations on the received request
///         assert_eq!(request.uri(), "http://example.com");
///         // Send a response back
///         response.send_response(http::Response::builder()
///             .status(200)
///             .body("response body")
///             .unwrap());
///     })
///     .await?;
///
/// // Verify the response
/// assert_eq!(response.status(), 200);
/// # Ok(())
/// # }
/// ```
pub struct TowerTest {
    timeout_duration: Duration,
}

impl TowerTest {
    /// Create a new test layer builder with default settings
    pub fn builder() -> Self {
        Self {
            timeout_duration: Duration::from_secs(1),
        }
    }

    /// Set the timeout duration for tests (default: 1 second)
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout_duration = duration;
        self
    }

    /// Set the layer to test and return a configured builder
    pub fn layer<L>(self, layer: L) -> LayerTestBuilderWithLayer<L> {
        LayerTestBuilderWithLayer {
            layer,
            timeout_duration: self.timeout_duration,
        }
    }
}

impl Default for TowerTest {
    fn default() -> Self {
        Self::builder()
    }
}

/// Tower test builder configured with a specific layer
///
/// This builder is returned after calling [`TowerTest::layer`] and provides
/// methods to execute tests with the configured layer. It supports both
/// oneshot tests and custom test scenarios.
pub struct LayerTestBuilderWithLayer<L> {
    layer: L,
    timeout_duration: Duration,
}

impl<L> LayerTestBuilderWithLayer<L> {
    /// Set the timeout duration for tests
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout_duration = duration;
        self
    }

    /// Execute a oneshot test with the given request and expectations
    ///
    /// The expectations closure receives a mock handle where you can set up
    /// the expected downstream behavior. Types are automatically inferred!
    pub async fn oneshot<Req, Resp, TestReq, TestResp, E, F, Fut>(
        self,
        request: TestReq,
        expectations: F,
    ) -> Result<TestResp, Box<dyn std::error::Error + Send + Sync>>
    where
        Req: Send + 'static,
        Resp: Send + 'static,
        L: Layer<::tower_test::mock::Mock<Req, Resp>>,
        L::Service: Service<TestReq, Response = TestResp, Error = E> + Send + 'static,
        <L::Service as Service<TestReq>>::Future: Send,
        F: FnOnce(::tower_test::mock::Handle<Req, Resp>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
        E: Into<Box<dyn std::error::Error + Send + Sync>> + Send + Sync + 'static,
        TestReq: Send + 'static,
        TestResp: Send + 'static,
    {
        let (mock_service, handle) = ::tower_test::mock::pair::<Req, Resp>();
        let service = self.layer.layer(mock_service);

        // Spawn the expectations handler with panic catching
        let expectations_handle = tokio::spawn(async move {
            // Wrap in panic handling
            let panic_result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| expectations(handle)));

            let future = match panic_result {
                Ok(fut) => fut,
                Err(panic_info) => {
                    let msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                        s.clone()
                    } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                        s.to_string()
                    } else {
                        "Unknown panic in expectations".to_string()
                    };
                    panic!("Expectations panicked: {}", msg);
                }
            };

            future.await;
        });

        // Run the test with timeout
        let test_result =
            tokio::time::timeout(self.timeout_duration, service.oneshot(request)).await;

        // Check if expectations task completed successfully
        match expectations_handle.await {
            Err(join_error) => {
                if join_error.is_panic() {
                    return Err("Expectations task panicked".into());
                } else {
                    return Err("Expectations task failed".into());
                }
            }
            Ok(_) => {}
        }

        match test_result {
            Ok(result) => result.map_err(|e| e.into()),
            Err(_) => {
                Err("Test timed out - this usually means unexpected requests or deadlock".into())
            }
        }
    }

    /// Execute a custom test with the given test function and expectations
    ///
    /// This provides more control over the service interaction for complex scenarios.
    /// The expectations closure receives a mock handle where you can set up
    /// the expected downstream behavior. Types are automatically inferred!
    pub async fn test<Req, Resp, TestResp, E, F, G, Fut1, Fut2>(
        self,
        test_fn: G,
        expectations: F,
    ) -> Result<TestResp, Box<dyn std::error::Error + Send + Sync>>
    where
        Req: Send + 'static,
        Resp: Send + 'static,
        L: Layer<::tower_test::mock::Mock<Req, Resp>>,
        L::Service: Send + 'static,
        F: FnOnce(::tower_test::mock::Handle<Req, Resp>) -> Fut1 + Send + 'static,
        G: FnOnce(L::Service) -> Fut2 + Send + 'static,
        Fut1: Future<Output = ()> + Send + 'static,
        Fut2: Future<Output = Result<TestResp, E>> + Send + 'static,
        E: Into<Box<dyn std::error::Error + Send + Sync>> + Send + Sync + 'static,
        TestResp: Send + 'static,
    {
        let (mock_service, handle) = ::tower_test::mock::pair::<Req, Resp>();
        let service = self.layer.layer(mock_service);

        // Spawn the expectations handler with panic catching
        let expectations_handle = tokio::spawn(async move {
            let result = catch_unwind(AssertUnwindSafe(|| expectations(handle)));

            match result {
                Ok(future) => {
                    future.await;
                }
                Err(panic_info) => {
                    if let Some(s) = panic_info.downcast_ref::<String>() {
                        panic!("Expectations panicked: {}", s);
                    } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                        panic!("Expectations panicked: {}", s);
                    } else {
                        panic!("Expectations panicked with unknown panic info");
                    }
                }
            }
        });

        // Run the test with timeout
        let test_result = tokio::time::timeout(self.timeout_duration, test_fn(service)).await;

        // Check if expectations task completed successfully
        match expectations_handle.await {
            Err(join_error) => {
                if join_error.is_panic() {
                    return Err("Expectations task panicked".into());
                } else {
                    return Err("Expectations task failed".into());
                }
            }
            Ok(_) => {}
        }

        match test_result {
            Ok(result) => result.map_err(|e| e.into()),
            Err(_) => {
                Err("Test timed out - this usually means unexpected requests or deadlock".into())
            }
        }
    }
}
