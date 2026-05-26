use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::time::Instant;

use opentelemetry::KeyValue;
use tower::Service;

use super::ServiceTarget;

/// Wraps a connector and records `apollo.router.connection.acquire.duration` each time a new
/// connection is established. Pool hits skip the connector entirely and are not recorded.
#[derive(Clone)]
pub(crate) struct ConnectionTimingConnector<C> {
    inner: C,
    metric_attributes: Arc<[KeyValue]>,
}

impl<C> ConnectionTimingConnector<C> {
    pub(in crate::services::http) fn new(
        inner: C,
        target: ServiceTarget,
        transport: &'static str,
    ) -> Self {
        let service_attribute = match target {
            ServiceTarget::Subgraph { name } => KeyValue::new("subgraph.name", name.to_string()),
            ServiceTarget::Connector { name } => {
                KeyValue::new("connector.source.name", name.to_string())
            }
            ServiceTarget::Coprocessor => KeyValue::new("coprocessor", true),
        };
        let metric_attributes = Arc::from(
            [
                KeyValue::new("network.transport", transport),
                service_attribute,
            ]
            .as_slice(),
        );
        Self {
            inner,
            metric_attributes,
        }
    }
}

impl<C, T> Service<T> for ConnectionTimingConnector<C>
where
    C: Service<T>,
    C::Future: Send + 'static,
    C::Response: Send + 'static,
    C::Error: Send + 'static,
{
    type Response = C::Response;
    type Error = C::Error;
    type Future = Pin<Box<dyn Future<Output = Result<C::Response, C::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, target: T) -> Self::Future {
        let start = Instant::now();
        let attributes = self.metric_attributes.clone();
        let fut = self.inner.call(target);
        Box::pin(async move {
            let result = fut.await;
            f64_histogram_with_unit!(
                "apollo.router.connection.acquire.duration",
                "Time to establish a new connection to a service",
                "s",
                start.elapsed().as_secs_f64(),
                attributes.as_ref()
            );
            result
        })
    }
}
