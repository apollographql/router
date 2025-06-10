use super::{Error, Request, Response};
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full};
use tower::Service;

/// A Tower service that uses reqwest to execute HTTP requests
///
/// This service takes HTTP requests with `UnsyncBoxBody<Bytes, Infallible>` bodies
/// and executes them using a reqwest client, returning HTTP responses in the same format.
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
        
        // Convert body to bytes
        let body_bytes = match body.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(err) => {
                return Err(Error::InvalidRequest {
                    details: format!("Failed to collect request body: {}", err),
                });
            }
        };

        // Build reqwest request
        let mut reqwest_request = self
            .client
            .request(parts.method, uri.clone())
            .body(body_bytes);

        // Add headers
        for (name, value) in parts.headers {
            if let Some(name) = name {
                reqwest_request = reqwest_request.header(name, value);
            }
        }

        // Execute the request
        let reqwest_response = reqwest_request
            .send()
            .await
            .map_err(|err| Error::RequestFailed {
                source: Box::new(err),
                url: uri,
                method: method.to_string(),
            })?;

        // Convert reqwest::Response to http::Response
        let status = reqwest_response.status();
        let headers = reqwest_response.headers().clone();
        
        let response_bytes = reqwest_response
            .bytes()
            .await
            .map_err(|err| Error::ResponseProcessingFailed {
                source: Box::new(err),
                context: "Failed to read response body".to_string(),
            })?;

        let mut http_response = http::Response::builder()
            .status(status);

        // Add headers
        for (name, value) in &headers {
            http_response = http_response.header(name, value);
        }

        let body = UnsyncBoxBody::new(Full::new(response_bytes).map_err(|_| unreachable!()));
        
        http_response
            .body(body)
            .map_err(|err| Error::ResponseProcessingFailed {
                source: Box::new(err),
                context: "Failed to build HTTP response".to_string(),
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
    type Future = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

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