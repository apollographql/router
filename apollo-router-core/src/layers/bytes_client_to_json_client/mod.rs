use crate::services::bytes_client::{Request as BytesClientRequest, Response as BytesClientResponse};
use crate::services::json_client::{Request as JsonClientRequest, Response as JsonClientResponse};
use futures::StreamExt;
use std::pin::Pin;
use thiserror::Error;
use tower::BoxError;
use tower::{Layer, Service};

#[derive(Debug, Error)]
pub enum Error {
    /// Failed to serialize bytes to JSON: {0}
    #[error("Failed to serialize bytes to JSON: {0}")]
    JsonSerialization(#[from] serde_json::Error),
    
    /// Downstream service error: {0}
    #[error("Downstream service error: {0}")]
    Downstream(#[from] BoxError),
}

#[derive(Clone, Debug)]
pub struct BytesClientToJsonClientLayer;

impl<S> Layer<S> for BytesClientToJsonClientLayer {
    type Service = BytesClientToJsonClientService<S>;

    fn layer(&self, service: S) -> Self::Service {
        BytesClientToJsonClientService { inner: service }
    }
}

#[derive(Clone, Debug)]
pub struct BytesClientToJsonClientService<S> {
    inner: S,
}

impl<S> Service<BytesClientRequest> for BytesClientToJsonClientService<S>
where
    S: Service<JsonClientRequest, Response = JsonClientResponse> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
{
    type Response = BytesClientResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: BytesClientRequest) -> Self::Future {
        // Create an extended layer for the inner service
        let original_extensions = req.extensions;
        let extended_extensions = original_extensions.extend();

        // Convert bytes to JSON
        let json_body = match serde_json::from_slice(&req.body) {
            Ok(json) => json,
            Err(e) => return Box::pin(async move { Err(Error::JsonSerialization(e).into()) }),
        };

        let json_client_req = JsonClientRequest {
            extensions: extended_extensions,
            body: json_body,
        };

        let future = self.inner.call(json_client_req);

        Box::pin(async move {
            // Await the inner service call
            let json_resp = future.await.map_err(Into::into)?;

            // Transform JsonClientResponse back to BytesClientResponse
            // Convert JSON responses back to bytes
            let bytes_responses = json_resp.responses.map(|json_value| {
                match serde_json::to_vec(&json_value) {
                    Ok(bytes) => bytes::Bytes::from(bytes),
                    Err(_) => bytes::Bytes::from("{}"), // Fallback to empty JSON object on error
                }
            });

            let bytes_resp = BytesClientResponse {
                extensions: original_extensions,
                responses: Box::pin(bytes_responses),
            };

            Ok(bytes_resp)
        })
    }
}

#[cfg(test)]
mod tests; 