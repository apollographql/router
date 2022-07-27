//! Prevent mutations if the HTTP method is GET.
//!
//! See [`Layer`] and [`Service`] for more details.

use std::ops::ControlFlow;

use http::header::HeaderName;
use http::Method;
use http::StatusCode;
use tower::BoxError;
use tower::Layer;
use tower::Service;

use crate::graphql::Error;
use crate::json_ext::Object;
use crate::layers::sync_checkpoint::CheckpointService;
use crate::ExecutionRequest;
use crate::ExecutionResponse;

#[derive(Default)]
pub(crate) struct AllowOnlyHttpPostMutationsLayer {}

impl<S> Layer<S> for AllowOnlyHttpPostMutationsLayer
where
    S: Service<ExecutionRequest, Response = ExecutionResponse> + Send + 'static,
    <S as Service<ExecutionRequest>>::Future: Send + 'static,
    <S as Service<ExecutionRequest>>::Error: Into<BoxError> + Send + 'static,
{
    type Service = CheckpointService<S, ExecutionRequest>;

    fn layer(&self, service: S) -> Self::Service {
        CheckpointService::new(
            |req: ExecutionRequest| {
                if req.originating_request.method() != Method::POST
                    && req.query_plan.contains_mutations()
                {
                    let errors = vec![Error {
                        message: "Mutations can only be sent over HTTP POST".to_string(),
                        locations: Default::default(),
                        path: Default::default(),
                        extensions: Default::default(),
                    }];
                    let mut res = ExecutionResponse::builder()
                        .errors(errors)
                        .extensions(Object::default())
                        .status_code(StatusCode::METHOD_NOT_ALLOWED)
                        .context(req.context)
                        .build();
                    res.response.inner.headers_mut().insert(
                        "Allow".parse::<HeaderName>().unwrap(),
                        "POST".parse().unwrap(),
                    );
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
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;
    use crate::error::Error;
    use crate::graphql;
    use crate::graphql::Response;
    use crate::http_ext;
    use crate::plugin::test::MockExecutionService;
    use crate::query_planner::fetch::OperationKind;
    use crate::query_planner::PlanNode;
    use crate::query_planner::QueryPlan;

    #[tokio::test]
    async fn it_lets_http_post_queries_pass_through() {
        let mut mock_service = MockExecutionService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |_| Ok(ExecutionResponse::fake_builder().build()));

        let mock = mock_service.build();

        let mut service_stack = AllowOnlyHttpPostMutationsLayer::default().layer(mock);

        let http_post_query_plan_request = create_request(Method::POST, OperationKind::Query);

        let services = service_stack.ready().await.unwrap();
        services
            .call(http_post_query_plan_request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn it_lets_http_post_mutations_pass_through() {
        let mut mock_service = MockExecutionService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |_| Ok(ExecutionResponse::fake_builder().build()));

        let mock = mock_service.build();

        let mut service_stack = AllowOnlyHttpPostMutationsLayer::default().layer(mock);

        let http_post_query_plan_request = create_request(Method::POST, OperationKind::Mutation);

        let services = service_stack.ready().await.unwrap();
        services
            .call(http_post_query_plan_request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn it_lets_http_get_queries_pass_through() {
        let mut mock_service = MockExecutionService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |_| Ok(ExecutionResponse::fake_builder().build()));

        let mock = mock_service.build();

        let mut service_stack = AllowOnlyHttpPostMutationsLayer::default().layer(mock);

        let http_post_query_plan_request = create_request(Method::GET, OperationKind::Query);

        let services = service_stack.ready().await.unwrap();
        services
            .call(http_post_query_plan_request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn it_doesnt_let_non_http_post_mutations_pass_through() {
        let expected_error = Error {
            message: "Mutations can only be sent over HTTP POST".to_string(),
            locations: Default::default(),
            path: Default::default(),
            extensions: Default::default(),
        };
        let expected_status = StatusCode::METHOD_NOT_ALLOWED;
        let expected_allow_header = "POST";

        let mock = MockExecutionService::new().build();
        let mut service_stack = AllowOnlyHttpPostMutationsLayer::default().layer(mock);

        let forbidden_requests = [
            Method::GET,
            Method::HEAD,
            Method::OPTIONS,
            Method::PUT,
            Method::DELETE,
            Method::TRACE,
            Method::CONNECT,
            Method::PATCH,
        ]
        .into_iter()
        .map(|method| create_request(method, OperationKind::Mutation));

        let services = service_stack.ready().await.unwrap();

        for request in forbidden_requests {
            let mut actual_error = services.call(request).await.unwrap();

            assert_eq!(expected_status, actual_error.response.status());
            assert_eq!(
                expected_allow_header,
                actual_error.response.headers().get("Allow").unwrap()
            );
            assert_error_matches(&expected_error, actual_error.next_response().await.unwrap());
        }
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

        let request = http_ext::Request::fake_builder()
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
