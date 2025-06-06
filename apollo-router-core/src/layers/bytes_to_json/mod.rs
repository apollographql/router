use crate::services::bytes_server::{Request as BytesRequest, Response as BytesResponse};
use crate::services::json_server::{Request as JsonRequest, Response as JsonResponse};
use bytes::Bytes;
use futures::StreamExt;
use std::pin::Pin;
use thiserror::Error;
use tower::BoxError;
use tower::{Layer, Service};

#[derive(Debug, Error)]
pub enum Error {
    /// Failed to parse JSON from bytes: {0}
    #[error("Failed to parse JSON from bytes: {0}")]
    JsonDeserialization(#[from] serde_json::Error),
    /// Downstream service error: {0}
    #[error("Downstream service error: {0}")]
    Downstream(#[from] BoxError),
}

#[derive(Clone, Debug)]
pub struct BytesToJsonLayer;

impl<S> Layer<S> for BytesToJsonLayer {
    type Service = BytesToJsonService<S>;

    fn layer(&self, service: S) -> Self::Service {
        BytesToJsonService { inner: service }
    }
}

#[derive(Clone, Debug)]
pub struct BytesToJsonService<S> {
    inner: S,
}

impl<S> Service<BytesRequest> for BytesToJsonService<S>
where
    S: Service<JsonRequest, Response = JsonResponse> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
{
    type Response = BytesResponse;
    type Error = Error;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(|e| Error::Downstream(e.into()))
    }

    fn call(&mut self, req: BytesRequest) -> Self::Future {
        // Convert bytes to JSON synchronously - fail fast if invalid
        let json_body = match serde_json::from_slice(&req.body) {
            Ok(json) => json,
            Err(e) => return Box::pin(async move { Err(Error::JsonDeserialization(e)) }),
        };

        // Create an extended layer for the inner service
        let original_extensions = req.extensions;
        let extended_extensions = original_extensions.extend();

        let json_req = JsonRequest {
            extensions: extended_extensions,
            body: json_body,
        };

        // Call the inner service directly - no cloning needed
        let future = self.inner.call(json_req);

        Box::pin(async move {
            // Await the inner service call
            let json_resp = future.await.map_err(|e| Error::Downstream(e.into()))?;

            // Convert JSON response to bytes response
            let bytes_stream = json_resp.responses.map(|json_value| {
                match serde_json::to_vec(&json_value) {
                    Ok(bytes) => Bytes::from(bytes),
                    Err(_) => Bytes::from("{}"), // Fallback to empty JSON object on serialization error
                }
            });

            let bytes_resp = BytesResponse {
                extensions: original_extensions,
                responses: Box::pin(bytes_stream),
            };

            Ok(bytes_resp)
        })
    }
}

#[cfg(test)]
mod tests; 