//! Content negotiation layer for standardizing HTTP content type handling.

use std::task::Poll;

use bytes::Bytes;
use http::HeaderValue;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use http::response::Parts;
use mediatype::MediaType;
use tower::Layer;
use tower::Service;

use crate::error::FetchError;
use crate::graphql;
use crate::plugins::content_negotiation::APPLICATION_GRAPHQL_JSON;

/// Content types supported by the router for subgraph responses
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ContentType {
    ApplicationJson,
    ApplicationGraphqlResponseJson,
}

/// Accept header values for different request types
pub(crate) struct AcceptHeaders;

impl AcceptHeaders {
    /// Standard GraphQL accept header for subgraph requests
    pub(crate) const GRAPHQL: &'static str = "application/json, application/graphql-response+json";

    /// Callback protocol accept header for subscription callbacks  
    #[allow(dead_code)]
    pub(crate) const CALLBACK: &'static str = "application/json;callbackSpec=1.0";
}

/// Content negotiation layer that handles Accept headers and content type validation
#[derive(Clone)]
pub(crate) struct ContentNegotiationLayer {
    accept_header: HeaderValue,
}

impl ContentNegotiationLayer {
    /// Create a new content negotiation layer with standard GraphQL accept header
    pub(crate) fn new() -> Self {
        Self {
            accept_header: HeaderValue::from_static(AcceptHeaders::GRAPHQL),
        }
    }

    /// Create a new content negotiation layer with callback accept header
    #[allow(dead_code)]
    pub(crate) fn with_callback_accept() -> Self {
        Self {
            accept_header: HeaderValue::from_static(AcceptHeaders::CALLBACK),
        }
    }

    /// Create a new content negotiation layer with custom accept header
    #[allow(dead_code)]
    pub(crate) fn with_accept_header(accept_header: HeaderValue) -> Self {
        Self { accept_header }
    }
}

impl<S> Layer<S> for ContentNegotiationLayer {
    type Service = ContentNegotiationService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ContentNegotiationService {
            inner,
            accept_header: self.accept_header.clone(),
        }
    }
}

/// Content negotiation service that adds Accept headers to requests
#[derive(Clone)]
pub(crate) struct ContentNegotiationService<S> {
    inner: S,
    accept_header: HeaderValue,
}

impl<S> Service<crate::services::http::HttpRequest> for ContentNegotiationService<S>
where
    S: Service<crate::services::http::HttpRequest>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: crate::services::http::HttpRequest) -> Self::Future {
        // Add Accept header to the request if not already present
        if !request.http_request.headers().contains_key(ACCEPT) {
            request
                .http_request
                .headers_mut()
                .insert(ACCEPT, self.accept_header.clone());
        }

        self.inner.call(request)
    }
}

/// Validates the content type of a subgraph response
pub(crate) fn validate_content_type(
    service_name: &str,
    parts: &Parts,
) -> Result<ContentType, FetchError> {
    if let Some(raw_content_type) = parts.headers.get(CONTENT_TYPE) {
        let content_type = raw_content_type
            .to_str()
            .ok()
            .and_then(|str| MediaType::parse(str).ok());

        match content_type {
            Some(mime) if mime.ty.as_str() == "application" && mime.subty.as_str() == "json" => {
                Ok(ContentType::ApplicationJson)
            }
            Some(mime)
                if mime.ty.as_str() == "application"
                    && mime.subty.as_str() == "graphql-response"
                    && mime.suffix.map(|s| s.as_str()) == Some("json") =>
            {
                Ok(ContentType::ApplicationGraphqlResponseJson)
            }
            Some(mime) => Err(format!(
                "subgraph response contains unsupported content-type: {}",
                mime,
            )),
            None => Err(format!(
                "subgraph response contains invalid 'content-type' header value {:?}",
                raw_content_type,
            )),
        }
    } else {
        Err("subgraph response does not contain 'content-type' header".to_owned())
    }
    .map_err(|reason| FetchError::SubrequestHttpError {
        status_code: Some(parts.status.as_u16()),
        service: service_name.to_string(),
        reason: format!(
            "{}; expected content-type: application/json or content-type: {}",
            reason, APPLICATION_GRAPHQL_JSON
        ),
    })
}

/// Converts HTTP response to GraphQL response with content type validation
pub(crate) fn http_response_to_graphql(
    service_name: &str,
    content_type: Result<ContentType, FetchError>,
    body: Option<Result<Bytes, FetchError>>,
    parts: &Parts,
) -> graphql::Response {
    let mut graphql_response = match (content_type, body, parts.status.is_success()) {
        (Ok(ContentType::ApplicationGraphqlResponseJson), Some(Ok(body)), _)
        | (Ok(ContentType::ApplicationJson), Some(Ok(body)), true) => {
            // Application graphql json expects valid graphql response
            // Application json expects valid graphql response if 2xx
            tracing::debug_span!("parse_subgraph_response").in_scope(|| {
                graphql::Response::from_bytes(body).unwrap_or_else(|error| {
                    let error = FetchError::SubrequestMalformedResponse {
                        service: service_name.to_owned(),
                        reason: error.reason,
                    };
                    graphql::Response::builder()
                        .error(error.to_graphql_error(None))
                        .build()
                })
            })
        }
        (Ok(ContentType::ApplicationJson), Some(Ok(body)), false) => {
            // Application json does not expect a valid graphql response if not 2xx.
            // If parse fails then attach the entire payload as an error
            tracing::debug_span!("parse_subgraph_response").in_scope(|| {
                let mut original_response = String::from_utf8_lossy(&body).to_string();
                if original_response.is_empty() {
                    original_response = "<empty response body>".into()
                }
                graphql::Response::from_bytes(body).unwrap_or_else(|_error| {
                    graphql::Response::builder()
                        .error(
                            FetchError::SubrequestMalformedResponse {
                                service: service_name.to_string(),
                                reason: original_response,
                            }
                            .to_graphql_error(None),
                        )
                        .build()
                })
            })
        }
        (content_type, body, _) => {
            // Something went wrong, compose a response with errors if they are present
            let mut graphql_response = graphql::Response::builder().build();
            if let Err(err) = content_type {
                graphql_response.errors.push(err.to_graphql_error(None));
            }
            if let Some(Err(err)) = body {
                graphql_response.errors.push(err.to_graphql_error(None));
            }
            graphql_response
        }
    };

    // Add an error for response codes that are not 2xx
    if !parts.status.is_success() {
        let status = parts.status;
        graphql_response.errors.insert(
            0,
            FetchError::SubrequestHttpError {
                service: service_name.to_string(),
                status_code: Some(status.as_u16()),
                reason: format!(
                    "{}: {}",
                    status.as_str(),
                    status.canonical_reason().unwrap_or("Unknown")
                ),
            }
            .to_graphql_error(None),
        )
    }
    graphql_response
}

/// Header values commonly used for content negotiation
pub(crate) mod header_values {
    use http::HeaderValue;

    /// Standard JSON content type header value
    #[allow(dead_code)]
    pub(crate) static APPLICATION_JSON: HeaderValue = HeaderValue::from_static("application/json");

    /// Standard GraphQL accept header value  
    #[allow(dead_code)]
    pub(crate) static ACCEPT_GRAPHQL_JSON: HeaderValue =
        HeaderValue::from_static("application/json, application/graphql-response+json");

    /// Callback protocol accept header value
    #[allow(dead_code)]
    pub(crate) static CALLBACK_PROTOCOL_ACCEPT: HeaderValue =
        HeaderValue::from_static("application/json;callbackSpec=1.0");
}

#[cfg(test)]
mod tests {
    use http::Response;

    use super::*;

    #[test]
    fn test_validate_content_type_application_json() {
        let mut response = Response::new(());
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let result = validate_content_type("test_service", &response.into_parts().0);
        assert_eq!(result.unwrap(), ContentType::ApplicationJson);
    }

    #[test]
    fn test_validate_content_type_graphql_response_json() {
        let mut response = Response::new(());
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/graphql-response+json"),
        );

        let result = validate_content_type("test_service", &response.into_parts().0);
        assert_eq!(result.unwrap(), ContentType::ApplicationGraphqlResponseJson);
    }

    #[test]
    fn test_validate_content_type_unsupported() {
        let mut response = Response::new(());
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("text/html"));

        let result = validate_content_type("test_service", &response.into_parts().0);
        assert!(result.is_err());

        if let Err(FetchError::SubrequestHttpError { reason, .. }) = result {
            assert!(reason.contains("unsupported content-type"));
        } else {
            panic!("Expected SubrequestHttpError error");
        }
    }

    #[test]
    fn test_validate_content_type_missing_header() {
        let response = Response::new(());

        let result = validate_content_type("test_service", &response.into_parts().0);
        assert!(result.is_err());

        if let Err(FetchError::SubrequestHttpError { reason, .. }) = result {
            assert!(reason.contains("does not contain 'content-type' header"));
        } else {
            panic!("Expected SubrequestHttpError error");
        }
    }

    #[test]
    fn test_accept_headers_constants() {
        assert_eq!(
            AcceptHeaders::GRAPHQL,
            "application/json, application/graphql-response+json"
        );
        assert_eq!(AcceptHeaders::CALLBACK, "application/json;callbackSpec=1.0");
    }

    #[tokio::test]
    async fn test_content_negotiation_layer_integration() {
        use http::Request;
        use tower::Service;
        use tower::ServiceExt;

        use crate::Context;
        use crate::services::http::HttpRequest;

        // Create a mock service that verifies the Accept header was added
        let mock_service = tower::service_fn(move |req: HttpRequest| {
            // Store the request to verify the Accept header was added
            let headers = req.http_request.headers().clone();

            async move {
                // Verify Accept header was added
                assert!(headers.contains_key(ACCEPT));
                let accept_header = headers.get(ACCEPT).unwrap();
                assert_eq!(
                    accept_header,
                    &HeaderValue::from_static(
                        "application/json, application/graphql-response+json"
                    )
                );

                Ok::<crate::services::http::HttpResponse, tower::BoxError>(
                    crate::services::http::HttpResponse {
                        http_response: http::Response::builder()
                            .status(200)
                            .body(crate::services::router::body::empty())
                            .unwrap(),
                        context: Context::new(),
                    },
                )
            }
        });

        // Apply the content negotiation layer
        let mut service = ContentNegotiationLayer::new().layer(mock_service);

        // Create a test request without Accept header
        let http_request = Request::builder()
            .uri("http://example.com/graphql")
            .method("POST")
            .body(crate::services::router::body::empty())
            .unwrap();

        let request = HttpRequest {
            http_request,
            context: Context::new(),
        };

        // Verify the request doesn't have Accept header initially
        assert!(!request.http_request.headers().contains_key(ACCEPT));

        // Call the service - this should add the Accept header
        let response = service.ready().await.unwrap().call(request).await.unwrap();

        // The test passes if no assertion failed (Accept header was added and verified in mock service)
        assert_eq!(response.http_response.status(), 200);
    }
}
