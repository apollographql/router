//! Utilities which make it easy to test with [`crate::plugin`].

mod broken;
mod mock;
mod restricted;

#[cfg(test)]
pub use mock::connector::MockConnector;
pub use mock::subgraph::MockSubgraph;

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

/// Keeps `handle` alive with unlimited readiness and panics if the mock ever receives
/// a request.
///
/// For mocks that must report ready — so requests can reach the layers under test and
/// be rejected there — but must never actually be called. The handle lives in a
/// detached task for the rest of the test.
pub(crate) fn allow_and_assert_never_called<Req, Res>(
    mut handle: tower_test::mock::Handle<Req, Res>,
) where
    Req: Send + 'static,
    Res: Send + 'static,
{
    handle.allow(u64::MAX);
    tokio::spawn(async move {
        if handle.next_request().await.is_some() {
            panic!("mock service was called but should never be");
        }
    });
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
