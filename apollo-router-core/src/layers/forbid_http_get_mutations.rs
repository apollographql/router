use crate::{plugin_utils, ExecutionRequest, ExecutionResponse};
use futures::{future::BoxFuture, FutureExt};
use http::Method;
use serde_json_bytes::json;
use std::task::Poll;
use tower::{BoxError, Layer, Service};

#[derive(Clone, Default)]
pub struct ForbidHttpGetMutations {}

pub struct ForbidHttpGetMutationsService<S>
where
    S: Service<ExecutionRequest>,
{
    service: S,
    forbid_http_get_mutations: ForbidHttpGetMutations,
}

impl<S> ForbidHttpGetMutationsService<S>
where
    S: Service<ExecutionRequest>,
{
    pub fn new(service: S) -> Self {
        Self {
            service,
            forbid_http_get_mutations: ForbidHttpGetMutations {},
        }
    }
}

impl<S> Layer<S> for ForbidHttpGetMutations
where
    S: Service<ExecutionRequest, Response = ExecutionResponse>,
{
    type Service = ForbidHttpGetMutationsService<S>;

    fn layer(&self, service: S) -> Self::Service {
        ForbidHttpGetMutationsService {
            service,
            forbid_http_get_mutations: self.clone(),
        }
    }
}

impl<S> Service<ExecutionRequest> for ForbidHttpGetMutationsService<S>
where
    S: Service<ExecutionRequest, Response = ExecutionResponse, Error = BoxError>,
    S::Future: Send + 'static,
{
    type Response = <S as Service<ExecutionRequest>>::Response;

    type Error = <S as Service<ExecutionRequest>>::Error;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: ExecutionRequest) -> Self::Future {
        if req.context.request.method() != Method::GET {
            Box::pin(self.service.call(req))
        } else {
            Box::pin(self.service.call(req).map(|service_response| {
                service_response.map(|res| {
                    if contains_mutations(&res) {
                        plugin_utils::ExecutionResponse::builder()
                            .errors(vec![crate::Error {
                                message: "PersistedQueryNotFound".to_string(),
                                locations: Default::default(),
                                path: Default::default(),
                                extensions: serde_json_bytes::from_value(json!({
                                      "code": "PERSISTED_QUERY_NOT_FOUND",
                                      "exception": {
                                      "stacktrace": [
                                          "PersistedQueryNotFoundError: PersistedQueryNotFound",
                                      ],
                                  },
                                }))
                                .unwrap(),
                            }])
                            .context(res.context)
                            .build()
                            .into()
                    } else {
                        res
                    }
                })
            }))
        }
    }
}

fn contains_mutations(_res: &ExecutionResponse) -> bool {
    true
}

#[cfg(test)]
mod forbid_http_get_mutations_tests {
    use super::*;
    use crate::plugin_utils::{ExecutionRequest, ExecutionResponse, MockExecutionService};
    use http::StatusCode;
    use serde_json_bytes::json;
    use tower::ServiceExt;

    #[tokio::test]
    async fn it_lets_http_post_queries_pass_through() {
        let mut mock_service = plugin_utils::MockExecutionService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |_| Ok(plugin_utils::ExecutionResponse::builder().build().into()));

        let mock = mock_service.build();

        let mut service_stack = ForbidHttpGetMutations {}.layer(mock);

        let http_post_query_plan_request = plugin_utils::ExecutionRequest::builder().build().into(); // TODO: post + query

        let services = service_stack.ready().await.unwrap();
        services.call(http_post_query_plan_request).await.unwrap();
    }

    #[tokio::test]
    async fn it_lets_http_post_mutations_pass_through() {
        let mut mock_service = MockExecutionService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |_| Ok(plugin_utils::ExecutionResponse::builder().build().into()));

        let mock = mock_service.build();

        let mut service_stack = ForbidHttpGetMutations {}.layer(mock);

        let http_post_query_plan_request = plugin_utils::ExecutionRequest::builder().build().into(); // TODO: post + mutation

        let services = service_stack.ready().await.unwrap();
        services.call(http_post_query_plan_request).await.unwrap();
    }

    #[tokio::test]
    async fn it_lets_http_get_queries_pass_through() {
        let mut mock_service = MockExecutionService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |_| Ok(ExecutionResponse::builder().build().into()));

        let mock = mock_service.build();

        let mut service_stack = ForbidHttpGetMutations {}.layer(mock);

        let http_post_query_plan_request = ExecutionRequest::builder().build().into(); // TODO: get + query

        let services = service_stack.ready().await.unwrap();
        services.call(http_post_query_plan_request).await.unwrap();
    }

    #[tokio::test]
    async fn it_doesnt_let_http_get_mutations_pass_through() {
        let expected_error = crate::Error {
            message: "PersistedQueryNotFound".to_string(),
            locations: Default::default(),
            path: Default::default(),
            extensions: serde_json_bytes::from_value(json!({
                  "code": "PERSISTED_QUERY_NOT_FOUND",
                  "exception": {
                  "stacktrace": [
                      "PersistedQueryNotFoundError: PersistedQueryNotFound",
                  ],
              },
            }))
            .unwrap(),
        };
        let expected_status = StatusCode::METHOD_NOT_ALLOWED;
        let expected_allow_header = "POST";

        let mock = MockExecutionService::new().build();
        let mut service_stack = ForbidHttpGetMutations {}.layer(mock);

        let http_post_query_plan_request = ExecutionRequest::builder().build().into(); // TODO: get + query

        let services = service_stack.ready().await.unwrap();
        let actual_error = services.call(http_post_query_plan_request).await.unwrap();

        assert_eq!(expected_status, actual_error.response.status()); //todo
        assert_eq!(
            expected_allow_header,
            actual_error.response.headers().get("Allow").unwrap()
        ); //todo
        assert_error_matches(&expected_error, actual_error);
    }

    fn assert_error_matches(expected_error: &crate::Error, response: crate::ExecutionResponse) {
        assert_eq!(&response.response.body().errors[0], expected_error);
    }
}
