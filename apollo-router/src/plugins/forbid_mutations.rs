use std::ops::ControlFlow;

use http::StatusCode;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::error::Error;
use crate::json_ext::Object;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::register_plugin;
use crate::ExecutionRequest;
use crate::ExecutionResponse;

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
        &self,
        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
        if self.forbid {
            ServiceBuilder::new()
                .checkpoint(|req: ExecutionRequest| {
                    if req.query_plan.contains_mutations() {
                        let error = Error {
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
    use http::Method;
    use http::StatusCode;
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;
    use crate::graphql;
    use crate::graphql::Response;
    use crate::http_ext::Request;
    use crate::plugin::test::MockExecutionService;
    use crate::query_planner::fetch::OperationKind;
    use crate::query_planner::PlanNode;
    use crate::query_planner::QueryPlan;

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

        let _ = service_stack
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn it_doesnt_let_mutations_pass_through() {
        let expected_error = Error {
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

        let mut actual_error = service_stack.oneshot(request).await.unwrap();

        assert_eq!(expected_status, actual_error.response.status());
        assert_error_matches(&expected_error, actual_error.next_response().await.unwrap());
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

        let _ = service_stack
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();
    }

    fn assert_error_matches(expected_error: &Error, response: Response) {
        assert_eq!(&response.errors[0], expected_error);
    }

    fn create_request(method: Method, operation_kind: OperationKind) -> crate::ExecutionRequest {
        let root: PlanNode = if operation_kind == OperationKind::Mutation {
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
            .body(graphql::Request::default())
            .build()
            .expect("expecting valid request");
        ExecutionRequest::fake_builder()
            .originating_request(request)
            .query_plan(QueryPlan::fake_builder().root(root).build())
            .build()
    }
}

register_plugin!("apollo", "forbid_mutations", ForbidMutations);
