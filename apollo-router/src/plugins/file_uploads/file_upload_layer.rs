use std::task::Context;
use std::task::Poll;

use futures::future::BoxFuture;
use http::HeaderName;
use http::HeaderValue;
use tower::BoxError;
use tower::Layer;
use tower::Service;

use super::multipart_form_data::MultipartFormData;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::router;

pub(super) static APOLLO_REQUIRE_PREFLIGHT: HeaderName =
    HeaderName::from_static("apollo-require-preflight");
pub(super) static TRUE: HeaderValue = HeaderValue::from_static("true");

pub(super) struct FileUploadLayer;

#[derive(Clone)]
pub(super) struct FileUploadService<S> {
    inner: S,
}

impl<S> Layer<S> for FileUploadLayer {
    type Service = FileUploadService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        FileUploadService { inner }
    }
}

impl<S> Service<HttpRequest> for FileUploadService<S>
where
    S: Service<HttpRequest, Response = HttpResponse, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = HttpResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<HttpResponse, BoxError>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: HttpRequest) -> Self::Future {
        let service = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, service);
        Box::pin(async move {
            let form = req
                .http_request
                .extensions_mut()
                .remove::<MultipartFormData>();
            if let Some(form) = form {
                let (mut parts, operations) = req.http_request.into_parts();
                parts
                    .headers
                    .insert(APOLLO_REQUIRE_PREFLIGHT.clone(), TRUE.clone());
                let body = router::body::from_result_stream(form.into_stream(operations).await);
                req.http_request = http::Request::from_parts(parts, body);
            }
            inner.call(req).await
        })
    }
}
