use crate::{
    register_plugin, ExecutionRequest, ExecutionResponse, Object, Plugin, ServiceBuilderExt,
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

    async fn new(forbid: Self::Config) -> Result<Self, BoxError> {
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
                        let error = crate::Error {
                            message: "Mutations are forbidden".to_string(),
                            locations: Default::default(),
                            path: Default::default(),
                            extensions: Default::default(),
                        };
                        let res = ExecutionResponse::builder()
                            .error(error)
                            .extensions(Object::new())
                            .status_code(StatusCode::BAD_REQUEST)
                            .context(req.context)
                            .build();
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

#[cfg(test)]
mod forbid_http_get_mutations_tests {
    use std::sync::Arc;

    use super::*;
    use crate::http_compat::Request;
    use crate::query_planner::fetch::OperationKind;
    use crate::{plugin::utils::test::MockExecutionService, QueryPlan};
    use http::{Method, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    #[tokio::test]
    async fn it_lets_queries_pass_through() {
        let mut mock_service = MockExecutionService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |_| Ok(ExecutionResponse::fake_builder().build()));

        let mock = mock_service.build();

        let service_stack = ForbidMutations::new(true)
            .await
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
            .await
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
            .returning(move |_| Ok(ExecutionResponse::fake_builder().build()));

        let mock = mock_service.build();

        let service_stack = ForbidMutations::new(false)
            .await
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

        let request = Request::fake_builder()
            .method(method)
            .body(crate::Request::default())
            .build()
            .expect("expecting valid request");
        ExecutionRequest::fake_builder()
            .originating_request(request)
            .query_plan(Arc::new(QueryPlan { root }))
            .build()
    }
}

register_plugin!("apollo", "forbid_mutations", ForbidMutations);
