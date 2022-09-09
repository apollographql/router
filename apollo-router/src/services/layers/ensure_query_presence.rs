//! Ensure that a [`SupergraphRequest`] contains a query.
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
use crate::SupergraphRequest;
use crate::SupergraphResponse;

#[derive(Default)]
pub(crate) struct EnsureQueryPresence {}

impl<S> Layer<S> for EnsureQueryPresence
where
    S: Service<SupergraphRequest, Response = SupergraphResponse> + Send + 'static,
    <S as Service<SupergraphRequest>>::Future: Send + 'static,
    <S as Service<SupergraphRequest>>::Error: Into<BoxError> + Send + 'static,
{
    type Service = CheckpointService<S, SupergraphRequest>;

    fn layer(&self, service: S) -> Self::Service {
        CheckpointService::new(
            |req: SupergraphRequest| {
                // A query must be available at this point
                let query = req.supergraph_request.body().query.as_ref();
                if query.is_none() || query.unwrap().trim().is_empty() {
                    let errors = vec![crate::error::Error {
                        message: "Must provide query string.".to_string(),
                        locations: Default::default(),
                        path: Default::default(),
                        extensions: Default::default(),
                    }];

                    //We do not copy headers from the request to the response as this may lead to leakable of sensitive data
                    let res = SupergraphResponse::builder()
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
    use crate::plugin::test::MockSupergraphService;

    #[tokio::test]
    async fn it_works_with_query() {
        let mut mock_service = MockSupergraphService::new();
        mock_service.expect_call().times(1).returning(move |_req| {
            Ok(SupergraphResponse::fake_builder()
                .build()
                .expect("expecting valid request"))
        });

        let service_stack = EnsureQueryPresence::default().layer(mock_service);

        let request: crate::SupergraphRequest = SupergraphRequest::fake_builder()
            .query("{__typename}".to_string())
            .build()
            .expect("expecting valid request");

        let _ = service_stack.oneshot(request).await.unwrap();
    }

    #[tokio::test]
    async fn it_fails_on_empty_query() {
        let expected_error = "Must provide query string.";

        let service_stack = EnsureQueryPresence::default().layer(MockSupergraphService::new());

        let request: crate::SupergraphRequest = SupergraphRequest::fake_builder()
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

        let service_stack = EnsureQueryPresence::default().layer(MockSupergraphService::new());

        let request: crate::SupergraphRequest = SupergraphRequest::fake_builder()
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
