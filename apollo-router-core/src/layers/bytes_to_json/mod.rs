use crate::services::bytes_server::{Request as BytesRequest, Response as BytesResponse};
use crate::services::json_server::{Request as JsonRequest, Response as JsonResponse};
use crate::json::JsonValue;
use bytes::Bytes;
use futures::StreamExt;
use std::pin::Pin;
use tower::BoxError;
use tower::{Layer, Service};

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
    S: Service<JsonRequest, Response = JsonResponse> + Clone + Send + 'static,
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
        // In case the inner service has state that's driven to readiness and
        // not tracked by clones (such as `Buffer`), pass the version we have
        // already called `poll_ready` on into the future, and leave its clone
        // behind.
        let mut inner = mem::replace(&mut self.inner, inner);

        Box::pin(async move {
            // Convert bytes to JSON
            let json_body: JsonValue = match serde_json::from_slice(&req.body) {
                Ok(json) => json,
                Err(e) => return Err(format!("Failed to parse JSON from bytes: {}", e).into()),
            };

            // Create an extended layer for the inner service
            let original_extensions = req.extensions;
            let extended_extensions = original_extensions.extend();

            let json_req = JsonRequest {
                extensions: extended_extensions,
                body: json_body,
            };

            // Call the inner service
            let json_resp = inner.call(json_req).await.map_err(Into::into)?;

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