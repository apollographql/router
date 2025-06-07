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

#[cfg(test)]
mod tests;

#[derive(Debug, Error)]
pub enum Error {
    /// Failed to build HTTP response: {0}
    #[error("Failed to build HTTP response: {0}")]
    HttpResponseBuilder(#[from] http::Error),
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
    type Error = BoxError;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: HttpRequest) -> Self::Future {
        use std::mem;

        let inner = self.inner.clone();
        // Clone is required here because we need to do async body collection
        // before calling the service, unlike bytes_to_json which can parse JSON synchronously
        let mut inner = mem::replace(&mut self.inner, inner);

        Box::pin(async move {
            // Convert HTTP request to bytes request
            let (parts, body) = req.into_parts();
            // Since body error type is now Infallible, collection cannot fail
            let body_bytes = body.collect().await.unwrap().to_bytes();

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
                body: body_bytes,
            };

            // Call the inner service
            let bytes_resp = inner.call(bytes_req).await.map_err(Into::into)?;

            // Convert bytes response to HTTP response
            let mut http_resp = http::Response::builder()
                .status(200)
                .body(UnsyncBoxBody::new(http_body_util::StreamBody::new(
                    bytes_resp.responses.map(|chunk| Ok(Frame::data(chunk))),
                )))
                .map_err(Error::HttpResponseBuilder)?;

            // Store the original Extensions back into the HTTP response extensions
            http_resp.extensions_mut().insert(original_extensions);

            Ok(http_resp)
        })
    }
}
