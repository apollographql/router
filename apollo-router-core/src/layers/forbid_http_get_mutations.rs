//! Prevent mutations if the HTTP method is GET.
//!
//! See [`Layer`] and [`Service`] for more details.

use crate::{checkpoint::CheckpointService, ExecutionRequest, ExecutionResponse, Object};
use http::{header::HeaderName, Method, StatusCode};
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
                if req.originating_request.method() == Method::GET
                    && req.query_plan.contains_mutations()
                {
                    let errors = vec![crate::Error {
                        message: "GET supports only query operation".to_string(),
                        locations: Default::default(),
                        path: Default::default(),
                        extensions: Default::default(),
                    }];
                    // let headers = vec![("Allow".to_string(), "POST".to_string())];
                    let mut res = ExecutionResponse::new(
                        None,
                        None,
                        None,
                        errors,
                        Object::new(),
                        Some(StatusCode::METHOD_NOT_ALLOWED),
                        req.context,
                    );
                    res.response
                        .inner
                        .headers_mut()
                        .insert::<HeaderName>("Allow".parse().unwrap(), "POST".parse().unwrap());
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

    use super::*;
    use crate::http_compat::RequestBuilder;
    use crate::query_planner::fetch::OperationKind;
    use crate::{plugin::utils::test::MockExecutionService, Context, QueryPlan};
    use http::{StatusCode, Uri};
    use serde_json::json;
    use std::str::FromStr;
    use std::sync::Arc;
    use tower::ServiceExt;

    #[tokio::test]
    async fn it_lets_http_post_queries_pass_through() {
        let mut mock_service = MockExecutionService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |_| Ok(ExecutionResponse::builder().build().into()));

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
            .returning(move |_| Ok(ExecutionResponse::builder().build().into()));

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

        let request = Arc::new(
            RequestBuilder::new(method, Uri::from_str("http://test").unwrap())
                .body(crate::Request::default())
                .unwrap(),
        );

        ExecutionRequest::builder()
            .originating_request(request)
            .query_plan(Arc::new(QueryPlan { root }))
            .context(Context::new())
            .build()
    }
}
