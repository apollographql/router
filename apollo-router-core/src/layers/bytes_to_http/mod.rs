use crate::services::bytes_client::{Request as BytesRequest, Response as BytesResponse};
use crate::services::http_client::{Request as HttpRequest, Response as HttpResponse};
use bytes::Bytes;
use futures::StreamExt;
use http_body_util::{BodyExt, combinators::UnsyncBoxBody};
use std::convert::Infallible;
use std::pin::Pin;
use thiserror::Error;
use tower::BoxError;
use tower::{Layer, Service};

// Type alias to match exactly what http_client uses
type HttpBody = UnsyncBoxBody<Bytes, Infallible>;

#[derive(Debug, Error)]
pub enum Error {
    /// Failed to build HTTP request: {0}
    #[error("Failed to build HTTP request: {0}")]
    HttpRequestBuilder(#[from] http::Error),
}

#[derive(Clone, Debug)]
pub struct BytesToHttpLayer;

impl BytesToHttpLayer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BytesToHttpLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for BytesToHttpLayer {
    type Service = BytesToHttpService<S>;

    fn layer(&self, service: S) -> Self::Service {
        BytesToHttpService { inner: service }
    }
}

#[derive(Clone, Debug)]
pub struct BytesToHttpService<S> {
    inner: S,
}

impl<S> Service<BytesRequest> for BytesToHttpService<S>
where
    S: Service<HttpRequest, Response = HttpResponse> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
{
    type Response = BytesResponse;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: BytesRequest) -> Self::Future {
        use std::mem;

        let inner = self.inner.clone();
        // Clone is required here because we need to handle async HTTP body collection
        let mut inner = mem::replace(&mut self.inner, inner);

        Box::pin(async move {
            // Create HTTP request
            let mut http_req = Self::create_http_request(req.body)?;

            // Store the original Extensions in the HTTP request extensions
            let original_extensions = req.extensions;
            let extended_extensions = original_extensions.extend();
            http_req.extensions_mut().insert(extended_extensions);

            // Call the inner service
            let http_resp = inner.call(http_req).await.map_err(Into::into)?;

            // Convert HTTP response to bytes response
            let (_parts, body) = http_resp.into_parts();

            // Since body error type is Infallible, collection cannot fail
            let collected_bytes = body.collect().await.unwrap().to_bytes();
            let bytes_stream = futures::stream::once(async move { collected_bytes });

            let bytes_resp = BytesResponse {
                extensions: original_extensions,
                responses: Box::pin(bytes_stream),
            };

            Ok(bytes_resp)
        })
    }
}

impl<S> BytesToHttpService<S> {
    /// Create HTTP request from bytes - helper to avoid lifetime issues
    fn create_http_request(body_bytes: Bytes) -> Result<HttpRequest, BoxError> {
        let full_body = http_body_util::Full::new(body_bytes);
        let body: HttpBody = UnsyncBoxBody::new(full_body);

        let http_req = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/json")
            .body(body)
            .map_err(Error::HttpRequestBuilder)?;

        Ok(http_req)
    }
}

#[cfg(test)]
mod tests;
