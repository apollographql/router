use crate::services::bytes_client::{Request as BytesRequest, Response as BytesResponse};
use crate::services::json_client::{Request as JsonRequest, Response as JsonResponse};
use bytes::Bytes;
use futures::StreamExt;
use std::pin::Pin;
use thiserror::Error;
use tower::BoxError;
use tower::{Layer, Service};

#[derive(Debug, Error)]
pub enum Error {
    /// Failed to serialize JSON to bytes: {0}
    #[error("Failed to serialize JSON to bytes: {0}")]
    JsonSerialization(#[from] serde_json::Error),
}

#[derive(Clone, Debug)]
pub struct JsonToBytesLayer;

impl<S> Layer<S> for JsonToBytesLayer {
    type Service = JsonToBytesService<S>;

    fn layer(&self, service: S) -> Self::Service {
        JsonToBytesService { inner: service }
    }
}

#[derive(Clone, Debug)]
pub struct JsonToBytesService<S> {
    inner: S,
}

impl<S> Service<JsonRequest> for JsonToBytesService<S>
where
    S: Service<BytesRequest, Response = BytesResponse> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
{
    type Response = JsonResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: JsonRequest) -> Self::Future {
        // Convert JSON to bytes synchronously - fail fast if invalid
        let bytes_body = match serde_json::to_vec(&req.body) {
            Ok(bytes) => Bytes::from(bytes),
            Err(e) => return Box::pin(async move { Err(Error::JsonSerialization(e).into()) }),
        };

        // Create an extended layer for the inner service
        let original_extensions = req.extensions;
        let extended_extensions = original_extensions.extend();

        let bytes_req = BytesRequest {
            extensions: extended_extensions,
            body: bytes_body,
        };

        // Call the inner service directly - no cloning needed
        let future = self.inner.call(bytes_req);

        Box::pin(async move {
            // Await the inner service call
            let bytes_resp = future.await.map_err(Into::into)?;

            // Convert bytes response to JSON response
            let json_stream = bytes_resp.responses.map(|bytes| {
                match serde_json::from_slice(&bytes) {
                    Ok(json_value) => json_value,
                    Err(_) => serde_json::Value::Object(serde_json::Map::new()), // Fallback to empty JSON object on deserialization error
                }
            });

            let json_resp = JsonResponse {
                extensions: original_extensions,
                responses: Box::pin(json_stream),
            };

            Ok(json_resp)
        })
    }
}

#[cfg(test)]
mod tests;
