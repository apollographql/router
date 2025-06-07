use crate::services::fetch::{Request as FetchRequest, Response as FetchResponse};
use crate::services::http_client::{Request as HttpClientRequest, Response as HttpClientResponse};
use std::pin::Pin;
use thiserror::Error;
use tower::BoxError;
use tower::{Layer, Service};

#[derive(Debug, Error)]
pub enum Error {
    /// Downstream service error: {0}
    #[error("Downstream service error: {0}")]
    Downstream(#[from] BoxError),
}

#[derive(Clone, Debug)]
pub struct FetchToHttpClientLayer;

impl<S> Layer<S> for FetchToHttpClientLayer {
    type Service = FetchToHttpClientService<S>;

    fn layer(&self, service: S) -> Self::Service {
        FetchToHttpClientService { inner: service }
    }
}

#[derive(Clone, Debug)]
pub struct FetchToHttpClientService<S> {
    inner: S,
}

impl<S> Service<FetchRequest> for FetchToHttpClientService<S>
where
    S: Service<HttpClientRequest, Response = HttpClientResponse> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
{
    type Response = FetchResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: FetchRequest) -> Self::Future {
        // Create an extended layer for the inner service
        let original_extensions = req.extensions;

        // Transform Fetch request to HTTP client request
        // For now, use placeholder URL and method - in real implementation,
        // this would be configured based on service registry/discovery
        let http_req = create_http_request(&req.service_name, &req.body, &req.variables);

        let http_client_req = http_req;

        let future = self.inner.call(http_client_req);

        Box::pin(async move {
            // Await the inner service call
            let http_resp = future.await.map_err(Into::into)?;

            // Transform HttpClientResponse back to FetchResponse
            // Convert the single HTTP response body into a JSON stream
            use http_body_util::BodyExt;
            let body_bytes = http_resp.into_body().collect().await
                .map_err(|_| "Failed to collect HTTP response body")?
                .to_bytes();
            
            // Parse as JSON and create a single-item stream
            let json_value: crate::json::JsonValue = serde_json::from_slice(&body_bytes)
                .unwrap_or(serde_json::json!({}));
            
            let response_stream = futures::stream::once(async move { json_value });
            
            let fetch_resp = FetchResponse {
                extensions: original_extensions,
                responses: Box::pin(response_stream),
            };

            Ok(fetch_resp)
        })
    }
}

fn create_http_request(
    service_name: &str,
    _body: &Box<dyn std::any::Any>,
    _variables: &std::collections::HashMap<String, crate::json::JsonValue>
) -> http::Request<http_body_util::combinators::UnsyncBoxBody<bytes::Bytes, std::convert::Infallible>> {
    use http_body_util::BodyExt;
    
    // Placeholder implementation - in real scenario, would create proper HTTP request
    // based on service registry, with appropriate headers, URL, method, etc.
    let body = http_body_util::Full::new(bytes::Bytes::from("{}")).boxed_unsync();
    
    http::Request::builder()
        .method(http::Method::POST)
        .uri(format!("http://{}", service_name))
        .header("content-type", "application/json")
        .body(body)
        .expect("Failed to build HTTP request")
}

#[cfg(test)]
mod tests; 