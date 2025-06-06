use crate::services::bytes_server::{Request as BytesRequest, Response as BytesResponse};
use crate::services::http_server::{Request as HttpRequest, Response as HttpResponse};
use futures::StreamExt;
use http_body::Frame;
use http_body_util::BodyExt;
use http_body_util::combinators::UnsyncBoxBody;
use std::pin::Pin;
use thiserror::Error;
use tower::BoxError;
use tower::{Layer, Service};

#[derive(Debug, Error)]
pub enum Error {
    /// Failed to collect HTTP body: {0}
    #[error("Failed to collect HTTP body: {0}")]
    HttpBodyCollection(tower::BoxError),
    /// Failed to build HTTP response: {0}
    #[error("Failed to build HTTP response: {0}")]
    HttpResponseBuilder(#[from] http::Error),
    /// Downstream service error: {0}
    #[error("Downstream service error: {0}")]
    Downstream(#[from] BoxError),
}

#[derive(Clone, Debug)]
pub struct HttpToBytesLayer;

impl<S> Layer<S> for HttpToBytesLayer {
    type Service = HttpToBytesService<S>;

    fn layer(&self, service: S) -> Self::Service {
        HttpToBytesService { inner: service }
    }
}

#[derive(Clone, Debug)]
pub struct HttpToBytesService<S> {
    inner: S,
}

impl<S> Service<HttpRequest> for HttpToBytesService<S>
where
    S: Service<BytesRequest, Response = BytesResponse> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
{
    type Response = HttpResponse;
    type Error = Error;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(|e| Error::Downstream(e.into()))
    }

    fn call(&mut self, req: HttpRequest) -> Self::Future {
        use std::mem;

        let inner = self.inner.clone();
        // In case the inner service has state that's driven to readiness and
        // not tracked by clones (such as `Buffer`), pass the version we have
        // already called `poll_ready` on into the future, and leave its clone
        // behind.
        let mut inner = mem::replace(&mut self.inner, inner);

        Box::pin(async move {
            // Convert HTTP request to bytes request
            let (parts, body) = req.into_parts();
            let body_bytes = body.collect().await.map_err(|e| Error::HttpBodyCollection(e.into()))?;

            // Extract our Extensions from the HTTP request extensions, or create default
            let original_extensions = parts
                .extensions
                .get::<crate::Extensions>()
                .cloned()
                .unwrap_or_default();

            // Create an extended layer for the inner service
            let extended_extensions = original_extensions.extend();

            let bytes_req = BytesRequest {
                extensions: extended_extensions,
                body: body_bytes.to_bytes(),
            };

            // Call the inner service
            let bytes_resp = inner.call(bytes_req).await.map_err(|e| Error::Downstream(e.into()))?;

            // Convert bytes response to HTTP response
            let mut http_resp = http::Response::builder()
                .status(200)
                .body(UnsyncBoxBody::new(http_body_util::StreamBody::new(
                    bytes_resp.responses.map(|chunk| Ok(Frame::data(chunk))),
                )))?;

            // Store the original Extensions back into the HTTP response extensions
            http_resp.extensions_mut().insert(original_extensions);

            Ok(http_resp)
        })
    }
}

#[cfg(test)]
mod tests;
