use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

use displaydoc::Display;
use pin_project_lite::pin_project;
use tower::Layer;
use tower_service::Service;

#[derive(thiserror::Error, Debug, Display, Clone)]
pub(super) enum HeaderLimitError {
    /// Request header too large
    HeaderTooLarge,
    /// Request header has too many list items
    HeaderListTooLarge,
}

pub(crate) struct HeaderLimitLayer {
    max_header_size: Option<usize>,
    max_header_list_items: Option<usize>,
}

impl HeaderLimitLayer {
    pub(crate) fn new(max_header_size: Option<usize>, max_header_list_items: Option<usize>) -> Self {
        Self {
            max_header_size,
            max_header_list_items,
        }
    }
}

impl<S> Layer<S> for HeaderLimitLayer {
    type Service = HeaderLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HeaderLimitService::new(inner, self.max_header_size, self.max_header_list_items)
    }
}

pub(crate) struct HeaderLimitService<S> {
    inner: S,
    max_header_size: Option<usize>,
    max_header_list_items: Option<usize>,
}

impl<S> HeaderLimitService<S> {
    fn new(inner: S, max_header_size: Option<usize>, max_header_list_items: Option<usize>) -> Self {
        Self {
            inner,
            max_header_size,
            max_header_list_items,
        }
    }

    fn validate_headers<Body>(&self, req: &http::Request<Body>) -> Result<(), HeaderLimitError> {
        for (name, value) in req.headers().iter() {
            // Check individual header size if limit is configured
            if let Some(max_size) = self.max_header_size {
                let header_size = name.as_str().len() + value.len();
                if header_size > max_size {
                    return Err(HeaderLimitError::HeaderTooLarge);
                }
            }

            // Check header list items if limit is configured
            if let Some(max_items) = self.max_header_list_items {
                if let Ok(value_str) = value.to_str() {
                    let item_count = value_str.split(',').count();
                    if item_count > max_items {
                        return Err(HeaderLimitError::HeaderListTooLarge);
                    }
                }
            }
        }
        Ok(())
    }
}

impl<ReqBody, RespBody, S> Service<http::Request<ReqBody>> for HeaderLimitService<S>
where
    S: Service<http::Request<ReqBody>, Response = http::Response<RespBody>>,
    S::Error: From<HeaderLimitError>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = HeaderLimitFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        // Validate headers before processing the request
        if let Err(e) = self.validate_headers(&req) {
            return HeaderLimitFuture::Reject { error: Some(e) };
        }

        // Headers are valid, proceed with the request
        let future = self.inner.call(req);
        HeaderLimitFuture::Continue { future }
    }
}

pin_project! {
    #[project = HeaderLimitFutureProj]
    pub(crate) enum HeaderLimitFuture<F> {
        Reject { error: Option<HeaderLimitError> },
        Continue { #[pin] future: F },
    }
}

impl<Inner, Body, Error> Future for HeaderLimitFuture<Inner>
where
    Inner: Future<Output = Result<http::response::Response<Body>, Error>>,
    Error: From<HeaderLimitError>,
{
    type Output = Result<http::response::Response<Body>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let project = self.project();
        match project {
            HeaderLimitFutureProj::Reject { error } => {
                if let Some(err) = error.take() {
                    Poll::Ready(Err(err.into()))
                } else {
                    // This shouldn't happen, but just in case
                    Poll::Ready(Err(HeaderLimitError::HeaderTooLarge.into()))
                }
            }
            HeaderLimitFutureProj::Continue { future } => future.poll(cx),
        }
    }
}

#[cfg(test)]
mod test {
    use http::StatusCode;
    use tower::BoxError;
    use tower::ServiceBuilder;
    use tower::ServiceExt;
    use tower_service::Service;

    use super::*;

    #[tokio::test]
    async fn test_header_size_limit_exceeded() {
        let mut service = ServiceBuilder::new()
            .layer(HeaderLimitLayer::new(Some(20), None))
            .service_fn(|_: http::Request<String>| async {
                panic!("should have rejected request");
            });

        let resp: Result<http::Response<String>, BoxError> = service
            .ready()
            .await
            .unwrap()
            .call(
                http::Request::builder()
                    .header("very-long-header-name", "very-long-header-value")
                    .body("test".to_string())
                    .unwrap(),
            )
            .await;

        assert!(resp.is_err());
    }

    #[tokio::test]
    async fn test_header_size_limit_ok() {
        let mut service = ServiceBuilder::new()
            .layer(HeaderLimitLayer::new(Some(50), None))
            .service_fn(|_: http::Request<String>| async {
                Ok(http::Response::builder()
                    .status(StatusCode::OK)
                    .body("success".to_string())
                    .unwrap())
            });

        let resp: Result<http::Response<String>, BoxError> = service
            .ready()
            .await
            .unwrap()
            .call(
                http::Request::builder()
                    .header("short", "value")
                    .body("test".to_string())
                    .unwrap(),
            )
            .await;

        assert!(resp.is_ok());
        let resp = resp.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.into_body(), "success");
    }

    #[tokio::test]
    async fn test_header_list_items_limit_exceeded() {
        let mut service = ServiceBuilder::new()
            .layer(HeaderLimitLayer::new(None, Some(2)))
            .service_fn(|_: http::Request<String>| async {
                panic!("should have rejected request");
            });

        let resp: Result<http::Response<String>, BoxError> = service
            .ready()
            .await
            .unwrap()
            .call(
                http::Request::builder()
                    .header("accept", "text/html, application/json, application/xml")
                    .body("test".to_string())
                    .unwrap(),
            )
            .await;

        assert!(resp.is_err());
    }

    #[tokio::test]
    async fn test_header_list_items_limit_ok() {
        let mut service = ServiceBuilder::new()
            .layer(HeaderLimitLayer::new(None, Some(3)))
            .service_fn(|_: http::Request<String>| async {
                Ok(http::Response::builder()
                    .status(StatusCode::OK)
                    .body("success".to_string())
                    .unwrap())
            });

        let resp: Result<http::Response<String>, BoxError> = service
            .ready()
            .await
            .unwrap()
            .call(
                http::Request::builder()
                    .header("accept", "text/html, application/json")
                    .body("test".to_string())
                    .unwrap(),
            )
            .await;

        assert!(resp.is_ok());
        let resp = resp.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.into_body(), "success");
    }

    #[tokio::test]
    async fn test_both_limits_ok() {
        let mut service = ServiceBuilder::new()
            .layer(HeaderLimitLayer::new(Some(50), Some(3)))
            .service_fn(|_: http::Request<String>| async {
                Ok(http::Response::builder()
                    .status(StatusCode::OK)
                    .body("success".to_string())
                    .unwrap())
            });

        let resp: Result<http::Response<String>, BoxError> = service
            .ready()
            .await
            .unwrap()
            .call(
                http::Request::builder()
                    .header("accept", "text/html, application/json")
                    .body("test".to_string())
                    .unwrap(),
            )
            .await;

        assert!(resp.is_ok());
        let resp = resp.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.into_body(), "success");
    }

    #[tokio::test]
    async fn test_no_limits_configured() {
        let mut service = ServiceBuilder::new()
            .layer(HeaderLimitLayer::new(None, None))
            .service_fn(|_: http::Request<String>| async {
                Ok(http::Response::builder()
                    .status(StatusCode::OK)
                    .body("success".to_string())
                    .unwrap())
            });

        let resp: Result<http::Response<String>, BoxError> = service
            .ready()
            .await
            .unwrap()
            .call(
                http::Request::builder()
                    .header("very-long-header-name-that-would-exceed-limits", "very-long-value-with-many-items,item1,item2,item3,item4")
                    .body("test".to_string())
                    .unwrap(),
            )
            .await;

        assert!(resp.is_ok());
        let resp = resp.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.into_body(), "success");
    }
}