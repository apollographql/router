use std::ops::ControlFlow;

use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::error::Error;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::execution;
use crate::services::ExecutionRequest;
use crate::services::ExecutionResponse;

#[derive(Debug, Clone)]
struct ForbidMutations {
    forbid: bool,
}

/// Forbid mutations configuration
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ForbidMutationsConfig(
    /// Enabled
    bool,
);

#[async_trait::async_trait]
impl Plugin for ForbidMutations {
    type Config = ForbidMutationsConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(ForbidMutations {
            forbid: init.config.0,
        })
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        if self.forbid {
            ServiceBuilder::new()
                .checkpoint(|req: ExecutionRequest| {
                    if req.query_plan.contains_mutations() {
                        let error = Error::builder()
                            .message("Mutations are forbidden".to_string())
                            .extension_code("MUTATION_FORBIDDEN")
                            .build();
                        let res = ExecutionResponse::builder()
                            .error(error)
                            .status_code(StatusCode::BAD_REQUEST)
                            .context(req.context)
                            .build()?;
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
    use crate::plugin::PluginInit;
    use crate::query_planner::fetch::OperationKind;
    use crate::query_planner::PlanNode;
    use crate::query_planner::QueryPlan;

    #[tokio::test]
    async fn it_lets_queries_pass_through() {
        let mut mock_service = MockExecutionService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |_| Ok(ExecutionResponse::fake_builder().build().unwrap()));

        let service_stack = ForbidMutations::new(PluginInit::fake_new(
            ForbidMutationsConfig(true),
            Default::default(),
        ))
        .await
        .expect("couldn't create forbid_mutations plugin")
        .execution_service(mock_service.boxed());

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
        let expected_error = Error::builder()
            .message("Mutations are forbidden".to_string())
            .extension_code("MUTATION_FORBIDDEN")
            .build();
        let expected_status = StatusCode::BAD_REQUEST;

        let service_stack = ForbidMutations::new(PluginInit::fake_new(
            ForbidMutationsConfig(true),
            Default::default(),
        ))
        .await
        .expect("couldn't create forbid_mutations plugin")
        .execution_service(MockExecutionService::new().boxed());
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
            .returning(move |_| Ok(ExecutionResponse::fake_builder().build().unwrap()));

        let service_stack = ForbidMutations::new(PluginInit::fake_new(
            ForbidMutationsConfig(false),
            Default::default(),
        ))
        .await
        .expect("couldn't create forbid_mutations plugin")
        .execution_service(mock_service.boxed());

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

    fn create_request(method: Method, operation_kind: OperationKind) -> ExecutionRequest {
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
            .supergraph_request(request)
            .query_plan(QueryPlan::fake_builder().root(root).build())
            .build()
    }
}

register_plugin!("apollo", "forbid_mutations", ForbidMutations);
