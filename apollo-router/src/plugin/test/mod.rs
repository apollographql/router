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
        .unwrap();
}

/// Assert that a mock service is never called during the test.
///
/// Waits up to 10 ms after the test action for a request to arrive; if one does,
/// the test fails immediately. Pass `mut handle` from `tower_test::mock::pair`.
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
