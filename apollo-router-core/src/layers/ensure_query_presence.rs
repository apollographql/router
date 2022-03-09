use crate::checkpoint::CheckpointService;
use crate::{plugin_utils, RouterRequest, RouterResponse};
use http::StatusCode;
use std::ops::ControlFlow;
use tower::{BoxError, Layer, Service};

#[derive(Default)]
pub struct EnsureQueryPresence {}

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
                let query = req.context.request.body().query.as_ref();
                if query.is_none() || query.unwrap().is_empty() {
                    let res = plugin_utils::RouterResponse::builder()
                        .errors(vec![crate::Error {
                            message: "Must provide query string.".to_string(),
                            locations: Default::default(),
                            path: Default::default(),
                            extensions: Default::default(),
                        }])
                        .context(req.context.into())
                        .build()
                        .with_status(StatusCode::BAD_REQUEST);
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
    use super::*;
    use crate::plugin_utils::MockRouterService;
    use crate::{plugin_utils, ResponseBody};
    use tower::ServiceExt;

    #[tokio::test]
    async fn it_works_with_query() {
        let mut mock_service = MockRouterService::new();
        mock_service
            .expect_call()
            .times(1)
            .returning(move |_req| Ok(plugin_utils::RouterResponse::builder().build().into()));

        let mock = mock_service.build();
        let service_stack = EnsureQueryPresence::default().layer(mock);

        let request: crate::RouterRequest = plugin_utils::RouterRequest::builder()
            .query("{__typename}".to_string())
            .build()
            .into();

        let _ = service_stack.oneshot(request).await.unwrap();
    }

    #[tokio::test]
    async fn it_fails_on_empty_query() {
        let expected_error = "Must provide query string.";

        let mock_service = MockRouterService::new();
        let mock = mock_service.build();

        let service_stack = EnsureQueryPresence::default().layer(mock);

        let request: crate::RouterRequest = plugin_utils::RouterRequest::builder()
            .query("".to_string())
            .build()
            .into();

        let response = service_stack
            .oneshot(request)
            .await
            .unwrap()
            .response
            .into_body();
        let actual_error = if let ResponseBody::GraphQL(b) = response {
            b.errors[0].message.clone()
        } else {
            panic!("response body should have been GraphQL");
        };

        assert_eq!(expected_error, actual_error);
    }

    #[tokio::test]
    async fn it_fails_on_no_query() {
        let expected_error = "Must provide query string.";

        let mock_service = MockRouterService::new();
        let mock = mock_service.build();
        let service_stack = EnsureQueryPresence::default().layer(mock);

        let request: crate::RouterRequest = plugin_utils::RouterRequest::builder().build().into();

        let response = service_stack
            .oneshot(request)
            .await
            .unwrap()
            .response
            .into_body();
        let actual_error = if let ResponseBody::GraphQL(b) = response {
            b.errors[0].message.clone()
        } else {
            panic!("response body should have been GraphQL");
        };
        assert_eq!(expected_error, actual_error);
    }
}
