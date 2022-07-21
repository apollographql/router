//! Ensure that a [`RouterRequest`] contains a query.
//!
//! See [`Layer`] and [`Service`] for more details.
//!
//! If the request does not contain a query, then the request is rejected.

use std::ops::ControlFlow;

use http::StatusCode;
use serde_json_bytes::Value;
use tower::BoxError;
use tower::Layer;
use tower::Service;

use crate::layers::sync_checkpoint::CheckpointService;
use crate::RouterRequest;
use crate::RouterResponse;

#[derive(Default)]
pub(crate) struct EnsureQueryPresence {}

impl<S> Layer<S> for EnsureQueryPresence
where
    S: Service<RouterRequest, Response = RouterResponse> + Send + 'static,
    <S as Service<RouterRequest>>::Future: Send + 'static,
    <S as Service<RouterRequest>>::Error: Into<BoxError> + Send + 'static,
{
    type Service = CheckpointService<S, RouterRequest>;

    fn layer(&self, service: S) -> Self::Service {
        CheckpointService::new(
            |req: RouterRequest| {
                // A query must be available at this point
                let query = req.originating_request.body().query.as_ref();
                if query.is_none() || query.unwrap().trim().is_empty() {
                    let errors = vec![crate::error::Error {
                        message: "Must provide query string.".to_string(),
                        locations: Default::default(),
                        path: Default::default(),
                        extensions: Default::default(),
                    }];

                    //We do not copy headers from the request to the response as this may lead to leakable of sensitive data
                    let res = RouterResponse::builder()
                        .data(Value::default())
                        .errors(errors)
                        .status_code(StatusCode::BAD_REQUEST)
                        .context(req.context)
                        .build()
                        .expect("response is valid");
                    Ok(ControlFlow::Break(res))
                } else {
                    Ok(ControlFlow::Continue(req))
                }
            },
            service,
        )
    }
}

#[cfg(test)]
mod ensure_query_presence_tests {
    use tower::ServiceExt;

    use super::*;
    use crate::plugin::test::MockRouterService;

    #[tokio::test]
    async fn it_works_with_query() {
        let mut mock_service = MockRouterService::new();
        mock_service.expect_call().times(1).returning(move |_req| {
            Ok(RouterResponse::fake_builder()
                .build()
                .expect("expecting valid request"))
        });

        let mock = mock_service.build();
        let service_stack = EnsureQueryPresence::default().layer(mock);

        let request: crate::RouterRequest = RouterRequest::fake_builder()
            .query("{__typename}".to_string())
            .build()
            .expect("expecting valid request");

        let _ = service_stack.oneshot(request).await.unwrap();
    }

    #[tokio::test]
    async fn it_fails_on_empty_query() {
        let expected_error = "Must provide query string.";

        let mock_service = MockRouterService::new();
        let mock = mock_service.build();

        let service_stack = EnsureQueryPresence::default().layer(mock);

        let request: crate::RouterRequest = RouterRequest::fake_builder()
            .query("".to_string())
            .build()
            .expect("expecting valid request");

        let response = service_stack
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();
        let actual_error = response.errors[0].message.clone();

        assert_eq!(expected_error, actual_error);
    }

    #[tokio::test]
    async fn it_fails_on_no_query() {
        let expected_error = "Must provide query string.";

        let mock_service = MockRouterService::new();
        let mock = mock_service.build();
        let service_stack = EnsureQueryPresence::default().layer(mock);

        let request: crate::RouterRequest = RouterRequest::fake_builder()
            .build()
            .expect("expecting valid request");

        let response = service_stack
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();
        let actual_error = response.errors[0].message.clone();
        assert_eq!(expected_error, actual_error);
    }
}
