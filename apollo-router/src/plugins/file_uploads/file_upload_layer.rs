use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;

use futures::TryStreamExt;
use futures::future::BoxFuture;
use http::HeaderName;
use http::HeaderValue;
use http::header::CONTENT_TYPE;
use tower::BoxError;
use tower::Layer;
use tower::Service;

use super::multipart_form_data::MultipartFormData;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::http::UploadStreamError;
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
            let upload_failed = Arc::new(AtomicBool::new(false));
            if let Some(form) = form {
                let (mut parts, operations) = req.http_request.into_parts();
                parts
                    .headers
                    .insert(APOLLO_REQUIRE_PREFLIGHT.clone(), TRUE.clone());
                parts.headers.insert(CONTENT_TYPE, form.content_type());
                let stream = form.into_stream(operations).await.inspect_err({
                    let upload_failed = upload_failed.clone();
                    move |_| upload_failed.store(true, Ordering::Relaxed)
                });
                let body = router::body::from_result_stream(stream);
                req.http_request = http::Request::from_parts(parts, body);
            }
            // An error from the client's upload stream fails the whole fetch. Say so, so that the
            // subgraph service can tell it apart from the subgraph failing.
            inner.call(req).await.map_err(|error| {
                if upload_failed.load(Ordering::Relaxed) {
                    UploadStreamError(error).into()
                } else {
                    error
                }
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;
    use indexmap::IndexMap;
    use tower::ServiceExt;

    use super::super::config::MultipartRequestLimits;
    use super::super::multipart_request::MultipartRequest;
    use super::*;
    use crate::Context;

    const BOUNDARY: &str = "boundary";

    /// A client's multipart upload of file `0`, with `file` as the part carrying it, or with no
    /// such part when `file` is empty.
    fn client_upload(file: &str) -> String {
        format!(
            "--{BOUNDARY}\r\n\
             Content-Disposition: form-data; name=\"operations\"\r\n\r\n\
             {{\"query\":\"mutation ($file: Upload!) {{ upload(file: $file) }}\",\"variables\":{{\"file\":null}}}}\r\n\
             --{BOUNDARY}\r\n\
             Content-Disposition: form-data; name=\"map\"\r\n\r\n\
             {{\"0\":[\"variables.file\"]}}\r\n\
             {file}\
             --{BOUNDARY}--\r\n"
        )
    }

    /// Sends the upload in `client_body` on to a subgraph through [`FileUploadLayer`], with
    /// `subgraph` standing in for the HTTP client below it, and returns the error the fetch failed
    /// with.
    async fn forward_upload<F>(
        client_body: String,
        subgraph: impl Fn(HttpRequest) -> F + Send + Clone + 'static,
    ) -> BoxError
    where
        F: Future<Output = Result<HttpResponse, BoxError>> + Send + 'static,
    {
        let mut multipart = MultipartRequest::new(
            router::body::from_bytes(client_body),
            BOUNDARY.to_string(),
            MultipartRequestLimits::default(),
            u64::MAX,
        );
        let operations = multipart.operations_field().await.expect("operations");
        operations.bytes().await.expect("operations are read");
        multipart.map_field().await.expect("map");
        let form = MultipartFormData::new(
            IndexMap::from([("0".to_string(), vec!["variables.file".to_string()])]),
            multipart,
        );

        let mut http_request = http::Request::new(router::body::from_bytes("{}"));
        http_request.extensions_mut().insert(form);
        FileUploadLayer
            .layer(tower::service_fn(subgraph))
            .oneshot(HttpRequest {
                http_request,
                context: Context::new(),
            })
            .await
            .err()
            .expect("the fetch should have failed")
    }

    /// Sends the request body on, the way the HTTP client does, and fails with its error if
    /// the body fails.
    async fn send_body(request: HttpRequest) -> Result<HttpResponse, BoxError> {
        request.http_request.into_body().collect().await?;
        Err("the subgraph went away".into())
    }

    /// A client upload that fails part way through fails the fetch with an [`UploadStreamError`],
    /// which reads like the error it wraps.
    #[tokio::test]
    async fn an_upload_that_fails_part_way_fails_the_fetch_as_an_upload_error() {
        let error = forward_upload(client_upload(""), send_body).await;

        assert!(error.is::<UploadStreamError>());
        assert_eq!(error.to_string(), "Missing files in the request: '0'.");
    }

    /// A fetch that fails after the whole upload was sent failed for some other reason, and
    /// keeps its own error.
    #[tokio::test]
    async fn a_fetch_that_fails_after_the_upload_keeps_its_error() {
        let file = format!(
            "--{BOUNDARY}\r\n\
             Content-Disposition: form-data; name=\"0\"; filename=\"a.txt\"\r\n\r\n\
             contents\r\n"
        );
        let error = forward_upload(client_upload(&file), send_body).await;

        assert!(!error.is::<UploadStreamError>());
        assert_eq!(error.to_string(), "the subgraph went away");
    }
}
