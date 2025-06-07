use crate::services::http_client::{Request as HttpClientRequest, Response as HttpClientResponse};
use crate::services::bytes_client::{Request as BytesClientRequest, Response as BytesClientResponse};
use bytes::Bytes;
use std::pin::Pin;
use thiserror::Error;
use tower::BoxError;
use tower::{Layer, Service};

#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum Error {
    /// HTTP request serialization failed
    #[error("HTTP request serialization failed")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_HTTP_CLIENT_TO_BYTES_CLIENT_REQUEST_SERIALIZATION_ERROR),
        help("Check that the HTTP request can be properly serialized to bytes")
    )]
    RequestSerialization {
        #[extension("context")]
        context: String,
        #[extension("details")]
        details: String,
    },

    /// HTTP response building failed
    #[error("HTTP response building failed")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_HTTP_CLIENT_TO_BYTES_CLIENT_RESPONSE_BUILD_ERROR),
        help("Check that the HTTP response can be properly constructed from bytes")
    )]
    ResponseBuilder {
        #[source]
        http_error: http::Error,
        #[extension("context")]
        context: String,
    },
}

#[derive(Clone, Debug)]
pub struct HttpClientToBytesClientLayer;

impl<S> Layer<S> for HttpClientToBytesClientLayer {
    type Service = HttpClientToBytesClientService<S>;

    fn layer(&self, service: S) -> Self::Service {
        HttpClientToBytesClientService { inner: service }
    }
}

#[derive(Clone, Debug)]
pub struct HttpClientToBytesClientService<S> {
    inner: S,
}

impl<S> Service<HttpClientRequest> for HttpClientToBytesClientService<S>
where
    S: Service<BytesClientRequest, Response = BytesClientResponse> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
{
    type Response = HttpClientResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: HttpClientRequest) -> Self::Future {
        // Convert HTTP request to bytes
        // For now, serialize as empty bytes - in real implementation,
        // would serialize the HTTP request headers, method, URI, and body
        let request_bytes = serialize_http_request(&req);

        let bytes_client_req = BytesClientRequest {
            extensions: crate::Extensions::default(),
            body: request_bytes,
        };

        let future = self.inner.call(bytes_client_req);

        Box::pin(async move {
            // Await the inner service call
            let _bytes_resp = future.await.map_err(Into::into)?;

            // Transform BytesClientResponse back to HttpClientResponse
            // For now, create a simple HTTP response - in real implementation,
            // would parse the bytes stream back into proper HTTP responses
            use http_body_util::BodyExt;
            let body = http_body_util::Full::new(bytes::Bytes::from("{}")).boxed_unsync();
            
            let http_resp = http::Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(body)
                .map_err(|http_error| Error::ResponseBuilder {
                    http_error,
                    context: "Building HTTP response from bytes in client layer".to_string(),
                })?;

            Ok(http_resp)
        })
    }
}

fn serialize_http_request(http_req: &http::Request<http_body_util::combinators::UnsyncBoxBody<bytes::Bytes, std::convert::Infallible>>) -> Bytes {
    // Placeholder implementation - in real scenario, would serialize
    // the HTTP request (method, URI, headers, body) into bytes
    let request_line = format!("{} {} HTTP/1.1\r\n", http_req.method(), http_req.uri());
    Bytes::from(request_line)
}



#[cfg(test)]
mod tests; 