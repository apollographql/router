//! Utilities which make it easy to test with [`crate::plugin`].

mod mock;
#[macro_use]
mod service;
mod broken;
mod restricted;

#[cfg(test)]
pub use mock::connector::MockConnector;
pub use mock::subgraph::MockSubgraph;
#[deprecated(
    note = "MockConnectorService is deprecated and will be removed in Router 3.0. \
            Use `tower_test::mock::pair` instead."
)]
pub use service::MockConnectorService;
#[deprecated(
    note = "MockExecutionService is deprecated and will be removed in Router 3.0. \
            Use `tower_test::mock::pair` instead."
)]
pub use service::MockExecutionService;
#[deprecated(
    note = "MockHttpClientService is deprecated and will be removed in Router 3.0. \
            Use `tower_test::mock::pair` instead."
)]
pub use service::MockHttpClientService;
#[deprecated(
    note = "MockRouterService is deprecated and will be removed in Router 3.0. \
            Use `tower_test::mock::pair` instead."
)]
pub use service::MockRouterService;
#[deprecated(
    note = "MockSubgraphService is deprecated and will be removed in Router 3.0. \
            Use `tower_test::mock::pair` instead."
)]
pub use service::MockSubgraphService;
#[deprecated(
    note = "MockSupergraphService is deprecated and will be removed in Router 3.0. \
            Use `tower_test::mock::pair` instead."
)]
pub use service::MockSupergraphService;

pub(crate) use self::mock::canned;

/// Await a mock service driver task, failing the test with a clear message if it
/// takes longer than 5 seconds or if the driver panicked (e.g. from an assertion).
///
/// Use this instead of `driver.await.unwrap()` so tests never hang silently.
#[cfg(test)]
pub(crate) async fn await_mock_driver(driver: tokio::task::JoinHandle<()>) {
    tokio::time::timeout(std::time::Duration::from_secs(5), driver)
        .await
        .expect("mock driver timed out — service was not called within 5 s")
        .expect("mock driver panicked");
}

/// Assert that a mock service is never called during the test.
///
/// Waits up to 10 ms after the test action for a request to arrive; if one does,
/// the test fails immediately. As the final action in your test, pass `handle`
/// from `tower_test::mock::pair`.
///
/// # Example
/// ```rust,ignore
/// use tower::ServiceBuilder;
/// use tower::Service as _;
/// use tower::ServiceExt as _;
/// let (mock, handle) = tower_test::mock::pair::<(), ()>();
/// let mut service = ServiceBuilder::new()
///     .filter(|_req| Err("never call inner"))
///     .service(mock);
/// service
///     .ready()
///     .await
///     .unwrap()
///     .call(())
///     .await.expect_err("should return error");
/// crate::plugin::test::assert_no_mock_calls(handle).await;
/// ```
#[cfg(test)]
pub(crate) async fn assert_no_mock_calls<Req, Res>(mut handle: tower_test::mock::Handle<Req, Res>)
where
    Req: Send + 'static,
    Res: Send + 'static,
{
    if matches!(
        tokio::time::timeout(std::time::Duration::from_millis(10), handle.next_request(),).await,
        Ok(Some(_))
    ) {
        panic!("mock service was called but should not have been");
    }
}
