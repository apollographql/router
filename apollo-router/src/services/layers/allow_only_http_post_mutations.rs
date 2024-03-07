//! Prevent mutations if the HTTP method is GET.
//!
//! See [`Layer`] and [`Service`] for more details.

use std::ops::ControlFlow;

use apollo_compiler::ast::OperationType;
use futures::future::BoxFuture;
use http::header::HeaderName;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceBuilder;

use super::query_analysis::ParsedDocument;
use crate::graphql::Error;
use crate::json_ext::Object;
use crate::layers::async_checkpoint::OneShotAsyncCheckpointService;
use crate::layers::ServiceBuilderExt;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;

#[derive(Default)]
pub(crate) struct AllowOnlyHttpPostMutationsLayer {}

impl<S> Layer<S> for AllowOnlyHttpPostMutationsLayer
where
    S: Service<SupergraphRequest, Response = SupergraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    <S as Service<SupergraphRequest>>::Future: Send + 'static,
{
    type Service = OneShotAsyncCheckpointService<
        S,
        BoxFuture<'static, Result<ControlFlow<SupergraphResponse, SupergraphRequest>, BoxError>>,
        SupergraphRequest,
    >;

    fn layer(&self, service: S) -> Self::Service {
        ServiceBuilder::new()
            .oneshot_checkpoint_async(|req: SupergraphRequest| {
                Box::pin(async {
                    if req.supergraph_request.method() == Method::POST {
                        return Ok(ControlFlow::Continue(req));
                    }

                    let doc = match req.context.extensions().lock().get::<ParsedDocument>() {
                        None => {
                            let errors = vec![Error::builder()
                                .message("Cannot find executable document".to_string())
                                .extension_code("MISSING_EXECUTABLE_DOCUMENT")
                                .build()];
                            let res = SupergraphResponse::infallible_builder()
                                .errors(errors)
                                .extensions(Object::default())
                                .status_code(StatusCode::INTERNAL_SERVER_ERROR)
                                .context(req.context.clone())
                                .build();

                            return Ok(ControlFlow::Break(res));
                        }
                        Some(c) => c.clone(),
                    };

                    let op = doc
                        .executable
                        .get_operation(req.supergraph_request.body().operation_name.as_deref());

                    match op {
                        Err(_) => {
                            let errors = vec![Error::builder()
                                .message("Cannot find operation".to_string())
                                .extension_code("MISSING_OPERATION")
                                .build()];
                            let res = SupergraphResponse::infallible_builder()
                                .errors(errors)
                                .extensions(Object::default())
                                .status_code(StatusCode::METHOD_NOT_ALLOWED)
                                .context(req.context)
                                .build();

                            Ok(ControlFlow::Break(res))
                        }
                        Ok(op) => {
                            if op.operation_type == OperationType::Mutation {
                                let errors = vec![Error::builder()
                                    .message(
                                        "Mutations can only be sent over HTTP POST".to_string(),
                                    )
                                    .extension_code("MUTATION_FORBIDDEN")
                                    .build()];
                                let mut res = SupergraphResponse::builder()
                                    .errors(errors)
                                    .extensions(Object::default())
                                    .status_code(StatusCode::METHOD_NOT_ALLOWED)
                                    .context(req.context)
                                    .build()?;
                                res.response.headers_mut().insert(
                                    HeaderName::from_static("allow"),
                                    HeaderValue::from_static("POST"),
                                );
                                Ok(ControlFlow::Break(res))
                            } else {
                                Ok(ControlFlow::Continue(req))
                            }
                        }
                    }
                })
                    as BoxFuture<
                        'static,
                        Result<ControlFlow<SupergraphResponse, SupergraphRequest>, BoxError>,
                    >
            })
            .service(service)
    }
}

#[cfg(test)]
mod forbid_http_get_mutations_tests {
    use std::sync::Arc;

    use apollo_compiler::ast;
    use tower::ServiceExt;

    use super::*;
    use crate::error::Error;
    use crate::graphql::Response;
    use crate::plugin::test::MockSupergraphService;
    use crate::query_planner::fetch::OperationKind;
    use crate::services::layers::query_analysis::ParsedDocumentInner;
    use crate::Context;

    #[tokio::test]
    async fn it_lets_http_post_queries_pass_through() {
        let mut mock_service = MockSupergraphService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |_| Ok(SupergraphResponse::fake_builder().build().unwrap()));

        let mut service_stack = AllowOnlyHttpPostMutationsLayer::default().layer(mock_service);

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
        let mut mock_service = MockSupergraphService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |_| Ok(SupergraphResponse::fake_builder().build().unwrap()));

        let mut service_stack = AllowOnlyHttpPostMutationsLayer::default().layer(mock_service);

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
        let mut mock_service = MockSupergraphService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |_| Ok(SupergraphResponse::fake_builder().build().unwrap()));

        let mut service_stack = AllowOnlyHttpPostMutationsLayer::default().layer(mock_service);

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
            extensions: serde_json_bytes::json!({
                "code": "MUTATION_FORBIDDEN"
            })
            .as_object()
            .unwrap()
            .to_owned(),
        };
        let expected_status = StatusCode::METHOD_NOT_ALLOWED;
        let expected_allow_header = "POST";

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

        for request in forbidden_requests {
            let mock_service = MockSupergraphService::new();
            let mut service_stack = AllowOnlyHttpPostMutationsLayer::default().layer(mock_service);
            let services = service_stack.ready().await.unwrap();

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

    fn create_request(method: Method, operation_kind: OperationKind) -> SupergraphRequest {
        let query = match operation_kind {
            OperationKind::Query => {
                "
                    type Query { a: Int }
                    query { a }
                "
            }
            OperationKind::Mutation => {
                "
                    type Query { a: Int }
                    type Mutation { a: Int }
                    mutation { a }
                "
            }
            OperationKind::Subscription => {
                "
                    type Query { a: Int }
                    type Subscription { a: Int }
                    subscription { a }
                "
            }
        };

        let ast = ast::Document::parse(query, "").unwrap();
        let (_schema, executable) = ast.to_mixed_validate().unwrap();
        let executable = executable.into_inner();

        let context = Context::new();
        context
            .extensions()
            .lock()
            .insert::<ParsedDocument>(Arc::new(ParsedDocumentInner {
                ast,
                executable: Arc::new(executable),
                parse_errors: None,
                validation_errors: None,
            }));

        SupergraphRequest::fake_builder()
            .method(method)
            .query(query)
            .context(context)
            .build()
            .unwrap()
    }
}
