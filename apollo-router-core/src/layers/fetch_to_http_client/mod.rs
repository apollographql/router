use crate::services::fetch::{Request as FetchRequest, Response as FetchResponse};
use crate::services::http_client::{Request as HttpClientRequest, Response as HttpClientResponse};
use std::pin::Pin;
use tower::BoxError;
use tower::{Layer, Service};

#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum Error {
    /// HTTP request building failed during fetch transformation
    #[error("HTTP request building failed during fetch transformation")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_FETCH_TO_HTTP_CLIENT_REQUEST_BUILD_ERROR),
        help("Check that the service name and fetch parameters are valid")
    )]
    HttpRequestBuilder {
        #[extension("serviceName")]
        service_name: String,
        #[extension("transformationContext")]
        context: String,
    },

    /// HTTP response body collection failed
    #[error("HTTP response body collection failed")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_FETCH_TO_HTTP_CLIENT_BODY_COLLECTION_ERROR),
        help("Check that the HTTP response body is valid and complete")
    )]
    BodyCollection {
        #[extension("collectionContext")]
        context: String,
    },

    /// JSON parsing failed during response transformation
    #[error("JSON parsing failed during response transformation")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_FETCH_TO_HTTP_CLIENT_JSON_PARSE_ERROR),
        help("Ensure the HTTP response contains valid JSON")
    )]
    JsonParse {
        #[source]
        json_error: serde_json::Error,
        #[source_code]
        response_data: Option<String>,
        #[extension("parseContext")]
        context: String,
    },
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
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

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
        let http_client_req =
            match create_http_request(&req.service_name, &req.body, &req.variables) {
                Ok(http_req) => http_req,
                Err(_) => {
                    let error = Error::HttpRequestBuilder {
                        service_name: req.service_name.clone(),
                        context: "Creating HTTP request for fetch operation".to_string(),
                    };
                    return Box::pin(async move { Err(error.into()) });
                }
            };

        let future = self.inner.call(http_client_req);

        Box::pin(async move {
            // Await the inner service call
            let http_resp = future.await.map_err(Into::into)?;

            // Transform HttpClientResponse back to FetchResponse
            // Convert the single HTTP response body into a JSON stream
            use http_body_util::BodyExt;
            let body_bytes = http_resp
                .into_body()
                .collect()
                .await
                .map_err(|_| Error::BodyCollection {
                    context: "Collecting HTTP response body for fetch transformation".to_string(),
                })?
                .to_bytes();

            // Parse as JSON and create a single-item stream
            let json_value: crate::json::JsonValue = match serde_json::from_slice(&body_bytes) {
                Ok(json) => json,
                Err(json_error) => {
                    return Err(Error::JsonParse {
                        json_error,
                        response_data: Some(String::from_utf8_lossy(&body_bytes).into_owned()),
                        context: "Parsing HTTP response body as JSON for fetch transformation"
                            .to_string(),
                    }
                    .into());
                }
            };

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
    _variables: &std::collections::HashMap<String, crate::json::JsonValue>,
) -> Result<
    http::Request<
        http_body_util::combinators::UnsyncBoxBody<bytes::Bytes, std::convert::Infallible>,
    >,
    http::Error,
> {
    use http_body_util::BodyExt;

    // Placeholder implementation - in real scenario, would create proper HTTP request
    // based on service registry, with appropriate headers, URL, method, etc.
    let body = http_body_util::Full::new(bytes::Bytes::from("{}")).boxed_unsync();

    http::Request::builder()
        .method(http::Method::POST)
        .uri(format!("http://{}", service_name))
        .header("content-type", "application/json")
        .body(body)
}

#[cfg(test)]
mod tests;
