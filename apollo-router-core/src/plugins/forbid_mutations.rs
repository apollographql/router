use crate::{
    plugin_utils, register_plugin, ExecutionRequest, ExecutionResponse, Plugin, ServiceBuilderExt,
};
use http::StatusCode;
use std::ops::ControlFlow;
use tower::util::BoxService;
use tower::{BoxError, ServiceBuilder, ServiceExt};

#[derive(Debug, Clone)]
struct ForbidMutations {
    forbid: bool,
}

#[async_trait::async_trait]
impl Plugin for ForbidMutations {
    type Config = bool;

    fn new(forbid: Self::Config) -> Result<Self, BoxError> {
        Ok(ForbidMutations { forbid })
    }

    fn execution_service(
        &mut self,
        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
        if self.forbid {
            ServiceBuilder::new()
                .checkpoint(|req: ExecutionRequest| {
                    if req.query_plan.contains_mutations() {
                        let res = plugin_utils::ExecutionResponse::builder()
                            .errors(vec![crate::Error {
                                message: "Mutations are forbidden".to_string(),
                                locations: Default::default(),
                                path: Default::default(),
                                extensions: Default::default(),
                            }])
                            .status(StatusCode::BAD_REQUEST)
                            .context(req.context)
                            .build()
                            .into();
                        Ok(ControlFlow::Break(res))
                    } else {
                        Ok(ControlFlow::Continue(req))
                    }
                })
                .service(service)
                .boxed()
        } else {
            service
        }
    }
}

register_plugin!("apollo", "forbid_mutations", ForbidMutations);

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
    use http::{Method, StatusCode, Uri};
    use serde_json::json;
    use std::str::FromStr;
    use tower::ServiceExt;

    #[tokio::test]
    async fn it_lets_queries_pass_through() {
        let mut mock_service = MockExecutionService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |_| Ok(ExecutionResponse::builder().build().into()));

        let mock = mock_service.build();

        let service_stack = ForbidMutations::new(true)
            .expect("couldnt' create forbid mutations plugin")
            .execution_service(mock.boxed());

        let request = create_request(Method::GET, OperationKind::Query);

        let _ = service_stack.oneshot(request).await.unwrap();
    }

    #[tokio::test]
    async fn it_doesnt_let_mutations_pass_through() {
        let expected_error = crate::Error {
            message: "Mutations are forbidden".to_string(),
            locations: Default::default(),
            path: Default::default(),
            extensions: Default::default(),
        };
        let expected_status = StatusCode::BAD_REQUEST;

        let mock = MockExecutionService::new().build();
        let service_stack = ForbidMutations::new(true)
            .expect("couldnt' create forbid mutations plugin")
            .execution_service(mock.boxed());
        let request = create_request(Method::GET, OperationKind::Mutation);

        let actual_error = service_stack.oneshot(request).await.unwrap();

        assert_eq!(expected_status, actual_error.response.status());
        assert_error_matches(&expected_error, actual_error);
    }

    #[tokio::test]
    async fn configuration_set_to_false_lets_mutations_pass_through() {
        let mut mock_service = MockExecutionService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |_| Ok(ExecutionResponse::builder().build().into()));

        let mock = mock_service.build();

        let service_stack = ForbidMutations::new(false)
            .expect("couldnt' create forbid mutations plugin")
            .execution_service(mock.boxed());

        let request = create_request(Method::GET, OperationKind::Mutation);

        let _ = service_stack.oneshot(request).await.unwrap();
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
                    RequestBuilder::new(method, Uri::from_str("http://test").unwrap())
                        .body(crate::Request::default())
                        .unwrap(),
                )),
            )
            .build()
            .into()
    }
}
