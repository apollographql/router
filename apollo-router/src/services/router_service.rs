//! Implements the router phase of the request lifecycle.

use std::task::Poll;

use bytes::Buf;
use futures::future::BoxFuture;
use futures::stream::StreamExt;
use http::Method;
use multimap::MultiMap;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;

use super::new_service::ServiceFactory;
use super::router;
use super::SupergraphCreator;
use crate::graphql;
use crate::router_factory::RouterFactory;
use crate::Endpoint;
use crate::ListenAddr;
use crate::RouterRequest;
use crate::RouterResponse;
use crate::SupergraphRequest;
use crate::SupergraphResponse;

/// Containing [`Service`] in the request lifecyle.
#[derive(Clone)]
pub(crate) struct RouterService {
    supergraph_creator: SupergraphCreator,
}

impl RouterService {
    pub(crate) fn new(supergraph_creator: SupergraphCreator) -> Self {
        RouterService { supergraph_creator }
    }
}

impl Service<RouterRequest> for RouterService {
    type Response = RouterResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: RouterRequest) -> Self::Future {
        let RouterRequest {
            router_request,
            context,
        } = req;

        let (parts, body) = router_request.into_parts();

        let supergraph_service = self.supergraph_creator.make();

        // TODO[igni]: deal with errors
        //     (StatusCode::BAD_REQUEST, "Invalid GraphQL request").into_response() will help
        let fut = async move {
            let graphql_request = if parts.method == Method::GET {
                parts
                    .uri
                    .query()
                    .and_then(|q| graphql::Request::from_urlencoded_query(q.to_string()).ok())
                    .unwrap()
            } else {
                let bytes = hyper::body::to_bytes(body).await.unwrap();
                serde_json::from_reader(bytes.reader()).unwrap()
            };

            let request = SupergraphRequest {
                supergraph_request: http::Request::from_parts(parts, graphql_request),
                context,
            };

            let SupergraphResponse { response, context } =
                supergraph_service.oneshot(request).await.unwrap();

            let (parts, body) = response.into_parts();

            Ok(RouterResponse {
                response: http::Response::from_parts(
                    parts,
                    hyper::Body::wrap_stream(body.map(|chunk| serde_json::to_vec(&chunk))),
                ),
                context,
            })
        };
        Box::pin(fut)
    }
}

/// A collection of services and data which may be used to create a "router".
#[derive(Clone)]
pub(crate) struct RouterCreator {
    supergraph_creator: SupergraphCreator,
}

impl ServiceFactory<router::Request> for RouterCreator {
    type Service = router::BoxService;
    fn create(&self) -> Self::Service {
        self.make().boxed()
    }
}

impl RouterFactory for RouterCreator {
    type RouterService = router::BoxService;

    type Future = <<RouterCreator as ServiceFactory<router::Request>>::Service as Service<
        router::Request,
    >>::Future;

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        let mut mm = MultiMap::new();
        self.supergraph_creator
            .plugins()
            .values()
            .for_each(|p| mm.extend(p.web_endpoints()));
        mm
    }
}

impl RouterCreator {
    pub(crate) fn new(supergraph_creator: SupergraphCreator) -> Self {
        Self { supergraph_creator }
    }
    pub(crate) fn make(
        &self,
    ) -> impl Service<
        router::Request,
        Response = router::Response,
        Error = BoxError,
        Future = BoxFuture<'static, router::ServiceResult>,
    > + Send {
        let router_service = RouterService::new(self.supergraph_creator.clone());

        ServiceBuilder::new().service(
            self.supergraph_creator
                .plugins()
                .iter()
                .rev()
                .fold(router_service.boxed(), |acc, (_, e)| e.router_service(acc)),
        )
    }

    /// Create a test service.
    #[cfg(test)]
    pub(crate) fn test_service(&self) -> router::BoxCloneService {
        use tower::buffer::Buffer;

        Buffer::new(self.make(), 512).boxed_clone()
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use crate::Context;

    use super::*;

    #[tokio::test]
    async fn it_extracts_query_and_operation_name_on_get_requests() {
        let query = "query";
        let expected_query = query;
        let operation_name = "operationName";
        let expected_operation_name = operation_name;

        let expected_response = graphql::Response::builder()
            .data(json!({"response": "yay"}))
            .build();
        let example_response = expected_response.clone();

        let mut expectations = MockSupergraphCreator::new();
        expectations
            .expect_service_call()
            .times(1)
            .returning(move |req| {
                let example_response = example_response.clone();
                Box::pin(async move {
                    let request: graphql::Request = serde_json::from_slice(
                        hyper::body::to_bytes(req.router_request.into_body())
                            .await
                            .unwrap()
                            .to_vec()
                            .as_slice(),
                    )
                    .unwrap();
                    assert_eq!(request.query.as_deref().unwrap(), expected_query);
                    assert_eq!(
                        request.operation_name.as_deref().unwrap(),
                        expected_operation_name
                    );
                    Ok(SupergraphResponse::new_from_graphql_response(
                        example_response,
                        Context::new(),
                    )
                    .into())
                })
            });

        let response = client
            .get(url.as_str())
            .query(&[("query", query), ("operationName", operation_name)])
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();

        assert_eq!(
            response.json::<graphql::Response>().await.unwrap(),
            expected_response,
        );

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn it_extracts_query_and_operation_name_on_post_requests() {
        let query = "query";
        let expected_query = query;
        let operation_name = "operationName";
        let expected_operation_name = operation_name;

        let expected_response = graphql::Response::builder()
            .data(json!({"response": "yay"}))
            .build();
        let example_response = expected_response.clone();

        let mut expectations = MockRouterService::new();
        expectations
            .expect_service_call()
            .times(1)
            .returning(move |req| {
                let example_response = example_response.clone();
                Box::pin(async move {
                    let request: graphql::Request = serde_json::from_slice(
                        hyper::body::to_bytes(req.router_request.into_body())
                            .await
                            .unwrap()
                            .to_vec()
                            .as_slice(),
                    )
                    .unwrap();
                    assert_eq!(request.query.as_deref().unwrap(), expected_query);
                    assert_eq!(
                        request.operation_name.as_deref().unwrap(),
                        expected_operation_name
                    );
                    Ok(SupergraphResponse::new_from_graphql_response(
                        example_response,
                        Context::new(),
                    )
                    .into())
                })
            });
        let (server, client) = init(expectations).await;
        let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());

        let response = client
            .post(url.as_str())
            .body(json!({ "query": query, "operationName": operation_name }).to_string())
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();

        assert_eq!(
            response.json::<graphql::Response>().await.unwrap(),
            expected_response,
        );

        server.shutdown().await
    }
}
