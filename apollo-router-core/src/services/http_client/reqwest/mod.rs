use futures::stream::TryStreamExt;
use http_body::Frame;
use http_body_util::BodyExt;
use http_body_util::StreamBody;
use http_body_util::combinators::UnsyncBoxBody;
use tower::Service;

use super::Error;
use super::Request;
use super::Response;

/// A Tower service that uses reqwest to execute HTTP requests
///
/// This service takes HTTP requests with `UnsyncBoxBody<Bytes, Infallible>` bodies
/// and executes them using a reqwest client, returning HTTP responses in the same format.
///
/// This implementation supports streaming of both request and response bodies to avoid
/// loading large payloads entirely into memory.
#[derive(Clone)]
pub struct ReqwestService {
    client: reqwest::Client,
}

impl ReqwestService {
    /// Create a new reqwest service with a default client
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    /// Create a new reqwest service with a custom reqwest client
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }

    /// Execute an HTTP request and return the response
    async fn execute_request(&self, request: Request) -> Result<Response, Error> {
        // Convert http::Request to reqwest::Request
        let (parts, body) = request.into_parts();

        // Clone method and URI for error handling
        let method = parts.method.clone();
        let uri = parts.uri.to_string();

        // Convert body to streaming reqwest body
        let body_stream = body
            .into_data_stream()
            .map_ok(|bytes| bytes) // bytes are already Bytes, no need to wrap in Frame
            .map_err(|_| std::io::Error::other("Failed to read request body frame"));

        let reqwest_body = reqwest::Body::wrap_stream(body_stream);

        // Build reqwest request with streaming body
        let mut reqwest_request = self
            .client
            .request(parts.method, uri.clone())
            .body(reqwest_body);

        // Add headers
        for (name, value) in parts.headers {
            if let Some(name) = name {
                reqwest_request = reqwest_request.header(name, value);
            }
        }

        // Execute the request
        let reqwest_response =
            reqwest_request
                .send()
                .await
                .map_err(|err| Error::RequestFailed {
                    source: Box::new(err),
                    url: uri,
                    method: method.to_string(),
                })?;

        // Convert reqwest::Response to http::Response with streaming body
        let status = reqwest_response.status();
        let headers = reqwest_response.headers().clone();

        // Create streaming response body
        let bytes_stream = reqwest_response.bytes_stream().map_ok(Frame::data).map_err(
            |err| -> Box<dyn std::error::Error + Send + Sync> {
                Box::new(Error::ResponseProcessingFailed {
                    source: Box::new(err),
                })
            },
        );

        let stream_body = StreamBody::new(bytes_stream);
        let body = UnsyncBoxBody::new(stream_body);

        let mut http_response = http::Response::builder().status(status);

        // Add headers
        for (name, value) in &headers {
            http_response = http_response.header(name, value);
        }

        http_response
            .body(body)
            .map_err(|err| Error::ResponseProcessingFailed {
                source: Box::new(err),
            })
    }
}

impl Default for ReqwestService {
    fn default() -> Self {
        Self::new()
    }
}

impl Service<Request> for ReqwestService {
    type Response = Response;
    type Error = Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        // reqwest client is always ready
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let client = self.clone();
        Box::pin(async move { client.execute_request(req).await })
    }
}

#[cfg(test)]
mod tests;
