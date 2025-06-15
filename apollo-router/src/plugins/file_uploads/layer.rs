use std::task::Poll;

use futures::future::BoxFuture;
use http::HeaderValue;
use http::header::CONTENT_TYPE;
use tower::BoxError;
use tower::Layer;
use tower::Service;

use super::multipart_form_data::MultipartFormData;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::router;

static APOLLO_REQUIRE_PREFLIGHT: http::HeaderName =
    http::HeaderName::from_static("apollo-require-preflight");
static TRUE: http::HeaderValue = HeaderValue::from_static("true");

/// Tower layer that handles file upload transformation for HTTP requests
#[derive(Clone)]
pub(crate) struct FileUploadLayer {
    enabled: bool,
}

impl FileUploadLayer {
    pub(crate) fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

impl<S> Layer<S> for FileUploadLayer {
    type Service = FileUploadService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        FileUploadService {
            inner,
            enabled: self.enabled,
        }
    }
}

/// Tower service that transforms HTTP requests containing multipart file uploads
#[derive(Clone)]
pub(crate) struct FileUploadService<S> {
    inner: S,
    enabled: bool,
}

impl<S> Service<HttpRequest> for FileUploadService<S>
where
    S: Service<HttpRequest, Response = HttpResponse, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = HttpResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: HttpRequest) -> Self::Future {
        if !self.enabled {
            return Box::pin(self.inner.call(req));
        }

        let mut inner = self.inner.clone();
        std::mem::swap(&mut inner, &mut self.inner);
        Box::pin(async move {
            // Transform the HTTP request if it contains multipart form data
            let transformed_req = Self::transform_multipart_request(req).await;
            inner.call(transformed_req).await
        })
    }
}

impl<S> FileUploadService<S> {
    /// Transforms an HTTP request with multipart form data for file uploads
    /// This is the equivalent of the original `http_request_wrapper` function
    async fn transform_multipart_request(req: HttpRequest) -> HttpRequest {
        let mut http_request = req.http_request;
        let form = http_request
            .extensions_mut()
            .get::<MultipartFormData>()
            .cloned();

        if let Some(form) = form {
            let (mut request_parts, operations) = http_request.into_parts();
            request_parts
                .headers
                .insert(APOLLO_REQUIRE_PREFLIGHT.clone(), TRUE.clone());

            // override Content-Type to be 'multipart/form-data'
            request_parts
                .headers
                .insert(CONTENT_TYPE, form.content_type());

            // Create the request body from the multipart form stream
            let request_body = router::body::from_result_stream(form.into_stream(operations).await);

            http_request = http::Request::from_parts(request_parts, request_body);
        }

        HttpRequest {
            http_request,
            context: req.context,
        }
    }
}
