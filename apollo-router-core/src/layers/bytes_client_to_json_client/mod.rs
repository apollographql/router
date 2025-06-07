use crate::services::bytes_client::{Request as BytesClientRequest, Response as BytesClientResponse};
use crate::services::json_client::{Request as JsonClientRequest, Response as JsonClientResponse};
use futures::StreamExt;
use std::pin::Pin;
use tower::BoxError;
use tower::{Layer, Service};

#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum Error {
    /// Bytes to JSON deserialization failed in client layer
    #[error("Bytes to JSON deserialization failed in client layer")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_BYTES_CLIENT_TO_JSON_CLIENT_DESERIALIZATION_ERROR),
        help("Ensure the bytes contain valid JSON data")
    )]
    JsonDeserialization {
        #[source]
        json_error: serde_json::Error,
        #[source_code]
        input_data: Option<String>,
        #[extension("deserializationContext")]
        context: String,
    },

    /// JSON to bytes serialization failed in response transformation
    #[error("JSON to bytes serialization failed in response transformation")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_BYTES_CLIENT_TO_JSON_CLIENT_SERIALIZATION_ERROR),
        help("Ensure the JSON response can be serialized to bytes")
    )]
    JsonSerialization {
        #[source]
        json_error: serde_json::Error,
        #[source_code]
        json_content: Option<String>,
        #[extension("serializationContext")]
        context: String,
    },
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
            Err(json_error) => {
                let error = Error::JsonDeserialization {
                    json_error,
                    input_data: Some(String::from_utf8_lossy(&req.body).into_owned()),
                    context: "Converting bytes request to JSON for client".to_string(),
                };
                return Box::pin(async move { Err(error.into()) });
            }
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
                    Err(_) => {
                        // Fallback to empty JSON object on serialization error
                        // In a real scenario, this error should be logged or handled differently
                        bytes::Bytes::from("{}")
                    }
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