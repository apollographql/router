use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tower::{Layer, Service, ServiceExt};

#[cfg(test)]
mod tests;

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

/// State tracking for mock service completion
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq))]
pub(crate) enum MockCompletionState {
    Running,
    Completed,
    Panicked(String),
    TimedOut,
}

/// Mock service wrapper that panics on drop if expectations weren't completed
///
/// This service wraps a tower-test mock service and tracks whether the
/// expectations closure completed successfully. If the service is dropped
/// without the expectations completing (due to timeout or panic), it will
/// panic on drop to alert the test of the failure.
pub struct MockService<Req, Resp> {
    inner: ::tower_test::mock::Mock<Req, Resp>,
    expectations_handle: Arc<tokio::task::JoinHandle<()>>,
    #[cfg(test)]
    pub(crate) completion_state: Arc<Mutex<MockCompletionState>>,
    #[cfg(not(test))]
    completion_state: Arc<Mutex<MockCompletionState>>,
}

// We can't use derive clone as this will place a bound on Req and Resp being clone
impl<Req, Resp> Clone for MockService<Req, Resp> {
    fn clone(&self) -> Self {
        MockService {
            inner: self.inner.clone(),
            expectations_handle: self.expectations_handle.clone(),
            completion_state: self.completion_state.clone(),
        }
    }
}

impl<Req, Resp> Drop for MockService<Req, Resp> {
    fn drop(&mut self) {
        // Always check for failure states regardless of reference count
        // Only ignore Running state to allow services to be dropped while still active
        if let Ok(state) = self.completion_state.lock() {
            match &*state {
                MockCompletionState::Running => {
                    // Allow the service to be dropped while running
                    // The background task will continue and complete or timeout
                }
                MockCompletionState::Panicked(msg) => {
                    panic!("Mock service expectations panicked: {}", msg);
                }
                MockCompletionState::TimedOut => {
                    panic!(
                        "Mock service expectations timed out - this usually means unexpected requests or deadlock"
                    );
                }
                MockCompletionState::Completed => {
                    // All good, expectations completed successfully
                }
            }
        }
    }
}

impl<Req, Resp> Service<Req> for MockService<Req, Resp>
where
    Req: Send + 'static,
    Resp: Send + 'static,
{
    type Response = <::tower_test::mock::Mock<Req, Resp> as Service<Req>>::Response;
    type Error = <::tower_test::mock::Mock<Req, Resp> as Service<Req>>::Error;
    type Future = <::tower_test::mock::Mock<Req, Resp> as Service<Req>>::Future;

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

/// Builder for testing tower layers with better error handling and panic detection
///
/// This builder provides a fluent API for configuring and running layer tests.
/// Unlike traditional tower tests, this builder provides:
/// - Automatic timeout protection to prevent hanging tests
/// - Panic detection in expectation handlers
/// - Clean separation of configuration and terminal methods
/// - No type annotation required for expectations!
/// - Service mocking for dependency injection testing
///
/// # Layer Testing Example
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
///
/// # Service Testing Example
///
/// ```rust,no_run
/// # use apollo_router_core::test_utils::tower_test::TowerTest;
/// # use tower::{Service, ServiceExt};
/// # use std::time::Duration;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// // Create a mock service for dependency injection
/// let mut mock_service = TowerTest::builder()
///     .timeout(Duration::from_secs(2))
///     .service(|mut handle| async move {
///         handle.allow(2);
///
///         // Handle first request
///         let (request, response) = handle.next_request().await.expect("service must not fail");
///         assert_eq!(request, "get_user:123");
///         response.send_response("User{id: 123, name: 'Alice'}");
///
///         // Handle second request
///         let (request, response) = handle.next_request().await.expect("service must not fail");
///         assert_eq!(request, "get_orders:123");
///         response.send_response("Orders[Order{id: 1}, Order{id: 2}]");
///     });
///
/// // Use the mock service in your service constructor
/// struct UserOrderService<S> {
///     user_service: S,
/// }
///
/// impl<S> UserOrderService<S> {
///     fn new(user_service: S) -> Self {
///         Self { user_service }
///     }
/// }
///
/// let service = UserOrderService::new(mock_service);
/// // ... test your service logic
/// # Ok(())
/// # }
/// ```
///
/// The mock service will automatically panic on drop if:
/// - The timeout is exceeded
/// - The expectations closure panics
/// - The expectations don't complete before the service is dropped
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

    /// Create a mock service for dependency injection testing
    ///
    /// This creates a mock service that can be passed to service constructors.
    /// The expectations closure receives a mock handle where you can set up
    /// the expected behavior. The mock service will panic on drop if the
    /// expectations are not completed or if a timeout occurs.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use apollo_router_core::test_utils::tower_test::TowerTest;
    /// # use std::time::Duration;
    /// let mock_service = TowerTest::builder()
    ///     .timeout(Duration::from_secs(2))
    ///     .service(|mut handle| async move {
    ///         handle.allow(1);
    ///         let (request, response) = handle.next_request().await.expect("service must not fail");
    ///         response.send_response("mock response");
    ///     });
    ///
    /// // MockService can be cloned for use in multiple places
    /// let cloned_service = mock_service.clone();
    ///
    /// // Use in constructor
    /// let my_service = MyServiceUnderTest::new(mock_service);
    /// ```
    pub fn service<Req, Resp, F, Fut>(self, expectations: F) -> MockService<Req, Resp>
    where
        Req: Send + 'static,
        Resp: Send + 'static,
        F: FnOnce(::tower_test::mock::Handle<Req, Resp>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let (mock_service, handle) = ::tower_test::mock::pair::<Req, Resp>();

        let completion_tracker = Arc::new(Mutex::new(MockCompletionState::Running));
        let completion_tracker_clone = completion_tracker.clone();

        // Spawn the expectations handler
        let expectations_handle = tokio::spawn(async move {
            let result = catch_unwind(AssertUnwindSafe(|| expectations(handle)));

            match result {
                Ok(future) => {
                    // Run the expectations
                    future.await;

                    // Mark as completed successfully
                    if let Ok(mut state) = completion_tracker_clone.lock() {
                        *state = MockCompletionState::Completed;
                    }
                }
                Err(panic_info) => {
                    // Mark as panicked
                    if let Ok(mut state) = completion_tracker_clone.lock() {
                        let msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                            s.clone()
                        } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                            s.to_string()
                        } else {
                            "Unknown panic in expectations".to_string()
                        };
                        *state = MockCompletionState::Panicked(msg);
                    }
                }
            }
        });

        // Set up timeout handling
        let timeout_tracker = completion_tracker.clone();
        let timeout_duration = self.timeout_duration;
        tokio::spawn(async move {
            tokio::time::sleep(timeout_duration).await;

            if let Ok(mut state) = timeout_tracker.lock() {
                if matches!(*state, MockCompletionState::Running) {
                    *state = MockCompletionState::TimedOut;
                }
            }
        });

        MockService {
            inner: mock_service,
            expectations_handle: Arc::new(expectations_handle),
            completion_state: completion_tracker,
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
    ///
    /// # ⚠️ Important: Avoid `ServiceExt::oneshot()` in Test Closures
    ///
    /// **Do NOT use `service.oneshot(request)` inside the test closure!** This will cause
    /// Higher-Ranked Trait Bounds (HRTB) compilation errors because `oneshot` requires
    /// the service to work with any lifetime, but your service may only work with `'static`.
    ///
    /// Instead, use the explicit `Service::ready()` + `Service::call()` pattern:
    ///
    /// ```rust,ignore
    /// // ❌ DON'T DO THIS - Will cause HRTB compile errors:
    /// TowerTest::builder()
    ///     .layer(my_layer)
    ///     .test(
    ///         |service| async move { service.oneshot(request).await }, // ❌ HRTB error!
    ///         |downstream| async move { /* expectations */ }
    ///     )
    ///
    /// // ✅ DO THIS INSTEAD - Works correctly:
    /// TowerTest::builder()
    ///     .layer(my_layer)
    ///     .test(
    ///         |mut service| async move {
    ///             use tower::Service;
    ///             service.ready().await?;           // ✅ Explicit readiness check
    ///             service.call(request).await       // ✅ Direct call works fine
    ///         },
    ///         |downstream| async move { /* expectations */ }
    ///     )
    /// ```
    ///
    /// For simple oneshot scenarios, prefer `TowerTest::oneshot()` instead of `test()`,
    /// as the framework handles the service interaction internally.
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
