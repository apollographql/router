use crate::services::bytes_server::{Request as BytesRequest, Response as BytesResponse};
use crate::services::json_server::{Request as JsonRequest, Response as JsonResponse};
use bytes::Bytes;
use futures::StreamExt;
use miette::SourceSpan;
use std::pin::Pin;
use tower::BoxError;
use tower::{Layer, Service};

#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum Error {
    /// Bytes to JSON conversion failed
    #[error("Bytes to JSON conversion failed")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_BYTES_TO_JSON_CONVERSION_ERROR),
        help("Ensure the input is valid JSON")
    )]
    JsonDeserialization {
        #[source]
        json_error: serde_json::Error,
        #[source_code]
        input_data: Option<String>,
        #[label("Invalid JSON")]
        error_position: Option<SourceSpan>,
    },
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
        // Convert bytes to JSON synchronously - fail fast if invalid
        let json_body = match serde_json::from_slice(&req.body) {
            Ok(json) => json,
            Err(json_error) => {
                let input_data = String::from_utf8_lossy(&req.body).into_owned();
                let error = Error::JsonDeserialization {
                    json_error,
                    input_data: Some(input_data),
                    error_position: None, // Could be enhanced with actual position parsing
                };
                return Box::pin(async move { Err(error.into()) });
            }
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
            let json_resp = future.await.map_err(Into::into)?;

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
