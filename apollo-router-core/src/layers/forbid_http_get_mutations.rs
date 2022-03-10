use crate::{checkpoint::CheckpointService, plugin_utils, ExecutionRequest, ExecutionResponse};
use http::{Method, StatusCode};
use std::ops::ControlFlow;
use tower::{BoxError, Layer, Service};

#[derive(Default)]
pub struct ForbidHttpGetMutationsLayer {}

impl<S> Layer<S> for ForbidHttpGetMutationsLayer
where
    S: Service<ExecutionRequest, Response = ExecutionResponse> + Send + 'static,
    <S as Service<ExecutionRequest>>::Future: Send + 'static,
    <S as Service<ExecutionRequest>>::Error: Into<BoxError> + Send + 'static,
{
    type Service = CheckpointService<S, ExecutionRequest>;

    fn layer(&self, service: S) -> Self::Service {
        CheckpointService::new(
            |req: ExecutionRequest| {
                if req.context.request.method() == Method::GET
                    && req.query_plan.contains_mutations()
                {
                    let res = plugin_utils::ExecutionResponse::builder()
                        .errors(vec![crate::Error {
                            message: "GET supports only query operation".to_string(),
                            locations: Default::default(),
                            path: Default::default(),
                            extensions: Default::default(),
                        }])
                        .status(StatusCode::METHOD_NOT_ALLOWED)
                        .headers(vec![("Allow".to_string(), "POST".to_string())])
                        .context(req.context)
                        .build()
                        .into();
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
mod forbid_http_get_mutations_tests {
    use std::sync::Arc;

    use super::*;
    use crate::http_compat::RequestBuilder;
    use crate::query_planner::fetch::OperationKind;
    use crate::{
        plugin_utils::{ExecutionRequest, ExecutionResponse, MockExecutionService},
        Context, QueryPlan,
    };
    use http::StatusCode;
    use reqwest::Url;
    use serde_json::json;
    use tower::ServiceExt;

    #[tokio::test]
    async fn it_lets_http_post_queries_pass_through() {
        let mut mock_service = plugin_utils::MockExecutionService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |_| Ok(plugin_utils::ExecutionResponse::builder().build().into()));

        let mock = mock_service.build();

        let mut service_stack = ForbidHttpGetMutationsLayer::default().layer(mock);

        let http_post_query_plan_request = create_request(Method::POST, OperationKind::Query);

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

        let mut service_stack = ForbidHttpGetMutationsLayer::default().layer(mock);

        let http_post_query_plan_request = create_request(Method::POST, OperationKind::Mutation);

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

        let mut service_stack = ForbidHttpGetMutationsLayer::default().layer(mock);

        let http_post_query_plan_request = create_request(Method::GET, OperationKind::Query);

        let services = service_stack.ready().await.unwrap();
        services.call(http_post_query_plan_request).await.unwrap();
    }

    #[tokio::test]
    async fn it_doesnt_let_http_get_mutations_pass_through() {
        let expected_error = crate::Error {
            message: "GET supports only query operation".to_string(),
            locations: Default::default(),
            path: Default::default(),
            extensions: Default::default(),
        };
        let expected_status = StatusCode::METHOD_NOT_ALLOWED;
        let expected_allow_header = "POST";

        let mock = MockExecutionService::new().build();
        let mut service_stack = ForbidHttpGetMutationsLayer::default().layer(mock);

        let http_post_query_plan_request = create_request(Method::GET, OperationKind::Mutation);

        let services = service_stack.ready().await.unwrap();
        let actual_error = services.call(http_post_query_plan_request).await.unwrap();

        assert_eq!(expected_status, actual_error.response.status());
        assert_eq!(
            expected_allow_header,
            actual_error.response.headers().get("Allow").unwrap()
        );
        assert_error_matches(&expected_error, actual_error);
    }

    fn assert_error_matches(expected_error: &crate::Error, response: crate::ExecutionResponse) {
        assert_eq!(&response.response.body().errors[0], expected_error);
    }

    fn create_request(method: Method, operation_kind: OperationKind) -> crate::ExecutionRequest {
        let root = if operation_kind == OperationKind::Mutation {
            serde_json::from_value(json!({
                "kind": "Sequence",
                "nodes": [
                    {
                        "kind": "Fetch",
                        "serviceName": "product",
                        "variableUsages": [],
                        "operation": "{__typename}",
                        "operationKind": "mutation"
                      },
                ]
            }))
            .unwrap()
        } else {
            serde_json::from_value(json!({
                "kind": "Sequence",
                "nodes": [
                    {
                        "kind": "Fetch",
                        "serviceName": "product",
                        "variableUsages": [],
                        "operation": "{__typename}",
                        "operationKind": "query"
                      },
                ]
            }))
            .unwrap()
        };

        ExecutionRequest::builder()
            .query_plan(Arc::new(QueryPlan { root }))
            .context(
                Context::new().with_request(Arc::new(
                    RequestBuilder::new(method, Url::parse("http://test").unwrap())
                        .body(crate::Request::default())
                        .unwrap(),
                )),
            )
            .build()
            .into()
    }
}
