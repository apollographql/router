//! Utilities which make it easy to test with [`crate::plugin`].

mod broken;
mod mock;
mod restricted;

#[cfg(test)]
pub use mock::connector::MockConnector;
pub use mock::subgraph::MockSubgraph;

#[cfg(any(test, feature = "mock_subgraphs_testing"))]
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

/// Wraps a service and counts every clone of it in a shared counter.
///
/// `BoxCloneService::clone` is deep, so a layer that clones its inner service per request clones
/// everything beneath it too. Placing this at the bottom of a stack counts those clones.
#[cfg(test)]
pub(crate) struct CountClones<S> {
    inner: S,
    clones: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
impl<S> CountClones<S> {
    pub(crate) fn new(inner: S, clones: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
        Self { inner, clones }
    }
}

#[cfg(test)]
impl<S: Clone> Clone for CountClones<S> {
    fn clone(&self) -> Self {
        self.clones
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self {
            inner: self.inner.clone(),
            clones: self.clones.clone(),
        }
    }
}

#[cfg(test)]
impl<S, Req> tower::Service<Req> for CountClones<S>
where
    S: tower::Service<Req>,
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
