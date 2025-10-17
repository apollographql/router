//! A supergraph service layer that requires that GraphQL mutations use the HTTP POST method.

use std::ops::ControlFlow;

use apollo_compiler::ast::OperationType;
use futures::future::BoxFuture;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use http::header::HeaderName;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceBuilder;

use super::query_analysis::ParsedDocument;
use crate::graphql::Error;
use crate::json_ext::Object;
use crate::layers::ServiceBuilderExt;
use crate::layers::async_checkpoint::AsyncCheckpointService;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;

/// A supergraph service layer that requires that GraphQL mutations use the HTTP POST method.
///
/// Responds with a 405 Method Not Allowed if it receives a GraphQL mutation using any other HTTP
/// method.
///
/// This layer requires that a ParsedDocument is available on the context and that the request has
/// a valid GraphQL operation and operation name. If these conditions are not met the layer will
/// return early with an unspecified error response.
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
    type Service = AsyncCheckpointService<
        S,
        BoxFuture<'static, Result<ControlFlow<SupergraphResponse, SupergraphRequest>, BoxError>>,
        SupergraphRequest,
    >;

    fn layer(&self, service: S) -> Self::Service {
        ServiceBuilder::new()
            .checkpoint_async(|req: SupergraphRequest| {
                Box::pin(async {
                    if req.supergraph_request.method() == Method::POST {
                        return Ok(ControlFlow::Continue(req));
                    }

                    let doc = match req
                        .context
                        .extensions()
                        .with_lock(|lock| lock.get::<ParsedDocument>().cloned())
                    {
                        None => {
                            // We shouldn't ever reach here unless the pipeline was set up
                            // improperly (i.e. programmer error), but do something better than
                            // panicking just in case.
                            let errors = vec![
                                Error::builder()
                                    .message("Cannot find executable document".to_string())
                                    .extension_code("MISSING_EXECUTABLE_DOCUMENT")
                                    .build(),
                            ];
                            let res = SupergraphResponse::infallible_builder()
                                .errors(errors)
                                .extensions(Object::default())
                                .status_code(StatusCode::INTERNAL_SERVER_ERROR)
                                .context(req.context.clone())
                                .build();

                            return Ok(ControlFlow::Break(res));
                        }
                        Some(c) => c,
                    };

                    let op = doc
                        .executable
                        .operations
                        .get(req.supergraph_request.body().operation_name.as_deref());

                    match op {
                        Err(_) => {
                            // We shouldn't end up here if the request is valid, and validation
                            // should happen well before this, but do something just in case.
                            let errors = vec![
                                Error::builder()
                                    .message("Cannot find operation".to_string())
                                    .extension_code("MISSING_OPERATION")
                                    .build(),
                            ];
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
                                let errors = vec![
                                    Error::builder()
                                        .message(
                                            "Mutations can only be sent over HTTP POST".to_string(),
                                        )
                                        .extension_code("MUTATION_FORBIDDEN")
                                        .build(),
                                ];
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
    use crate::Context;
    use crate::assert_error_eq_ignoring_id;
    use crate::error::Error;
    use crate::plugin::test::MockSupergraphService;
    use crate::query_planner::fetch::OperationKind;
    use crate::services::layers::query_analysis::ParsedDocumentInner;

    #[tokio::test]
    async fn it_lets_http_post_queries_pass_through() {
        let mut mock_service = MockSupergraphService::new();

        mock_service
            .expect_clone()
            .returning(MockSupergraphService::new);

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
            .expect_clone()
            .returning(MockSupergraphService::new);

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
            .expect_clone()
            .returning(MockSupergraphService::new);

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
        let expected_error = Error::builder()
            .message("Mutations can only be sent over HTTP POST".to_string())
            .extension_code("MUTATION_FORBIDDEN")
            .build();
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
            let mut mock_service = MockSupergraphService::new();

            mock_service
                .expect_clone()
                .returning(MockSupergraphService::new);

            let mut service_stack = AllowOnlyHttpPostMutationsLayer::default().layer(mock_service);
            let services = service_stack.ready().await.unwrap();

            let mut error_response = services.call(request).await.unwrap();
            let response = error_response.next_response().await.unwrap();

            assert_eq!(expected_status, error_response.response.status());
            assert_eq!(
                expected_allow_header,
                error_response.response.headers().get("Allow").unwrap()
            );
            assert_error_eq_ignoring_id!(expected_error, response.errors[0]);
        }
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

        let context = Context::new();
        context.extensions().with_lock(|lock| {
            lock.insert::<ParsedDocument>(
                ParsedDocumentInner::new(ast, Arc::new(executable), None, Default::default())
                    .unwrap(),
            )
        });

        SupergraphRequest::fake_builder()
            .method(method)
            .query(query)
            .context(context)
            .build()
            .unwrap()
    }
}
