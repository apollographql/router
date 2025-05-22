//! Extension of map_future layer. Allows mapping of the first graphql response. Useful when working with a stream of responses.
//!
//! See [`Layer`] and [`Service`] for more details.

use std::future::ready;
use std::task::Poll;

use futures::FutureExt;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::once;
use tower::Layer;
use tower::Service;

use crate::Context;
use crate::graphql;
use crate::services::supergraph;

/// [`Layer`] for mapping first graphql responses. See [`ServiceBuilderExt::map_first_graphql_response()`](crate::layers::ServiceBuilderExt::map_first_graphql_response()).
pub struct MapFirstGraphqlResponseLayer<Callback> {
    pub(super) callback: Callback,
}

/// [`Service`] for mapping first graphql responses. See [`ServiceBuilderExt::map_first_graphql_response()`](crate::layers::ServiceBuilderExt::map_first_graphql_response()).
pub struct MapFirstGraphqlResponseService<InnerService, Callback> {
    inner: InnerService,
    callback: Callback,
}

impl<InnerService, Callback> Layer<InnerService> for MapFirstGraphqlResponseLayer<Callback>
where
    Callback: Clone,
{
    type Service = MapFirstGraphqlResponseService<InnerService, Callback>;

    fn layer(&self, inner: InnerService) -> Self::Service {
        MapFirstGraphqlResponseService {
            inner,
            callback: self.callback.clone(),
        }
    }
}

impl<InnerService, Callback, Request> Service<Request>
    for MapFirstGraphqlResponseService<InnerService, Callback>
where
    InnerService: Service<Request, Response = supergraph::Response>,
    InnerService::Future: Send + 'static,
    Callback: FnOnce(
            Context,
            http::response::Parts,
            graphql::Response,
        ) -> (http::response::Parts, graphql::Response)
        + Clone
        + Send
        + 'static,
{
    type Response = supergraph::Response;
    type Error = InnerService::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let future = self.inner.call(request);
        let callback = self.callback.clone();
        async move {
            let supergraph_response = future.await?;
            let context = supergraph_response.context;
            let (mut parts, mut stream) = supergraph_response.response.into_parts();
            if let Some(first) = stream.next().await {
                let (new_parts, new_first) = callback(context.clone(), parts, first);
                parts = new_parts;
                stream = once(ready(new_first)).chain(stream).boxed();
            };
            Ok(supergraph::Response {
                context,
                response: http::Response::from_parts(parts, stream),
            })
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use tower::ServiceExt;

    use super::*;
    use crate::layers::ServiceExt as _;

    #[tokio::test]
    async fn test_map_first_graphql_response() {
        assert_eq!(
            crate::TestHarness::builder()
                .execution_hook(|service| {
                    service
                        .map_first_graphql_response(|_context, http_parts, mut graphql_response| {
                            graphql_response.errors.push(
                                graphql::Error::builder()
                                    .message("oh no!")
                                    .extension_code("FOO".to_string())
                                    .build(),
                            );
                            (http_parts, graphql_response)
                        })
                        .boxed()
                })
                .build_supergraph()
                .await
                .unwrap()
                .oneshot(supergraph::Request::canned_builder().build().unwrap())
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap()
                .errors[0]
                .message,
            "oh no!"
        );
    }
}
