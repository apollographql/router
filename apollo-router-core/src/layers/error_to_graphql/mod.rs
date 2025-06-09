use crate::services::json_server::{Response as JsonResponse, ResponseStream};
use apollo_router_error::HeapErrorToGraphQL;
use futures::stream;
use std::pin::Pin;
use tower::BoxError;
use tower::{Layer, Service};





#[derive(Clone, Debug)]
pub struct ErrorToGraphQLLayer;

impl<S> Layer<S> for ErrorToGraphQLLayer {
    type Service = ErrorToGraphQLService<S>;

    fn layer(&self, service: S) -> Self::Service {
        ErrorToGraphQLService { inner: service }
    }
}

#[derive(Clone, Debug)]
pub struct ErrorToGraphQLService<S> {
    inner: S,
}

impl<S, Req> Service<Req> for ErrorToGraphQLService<S>
where
    S: Service<Req, Response = JsonResponse> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    Req: Send + 'static,
{
    type Response = JsonResponse;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let future = self.inner.call(req);

        Box::pin(async move {
            match future.await {
                Ok(response) => {
                    // For successful responses, pass them through
                    Ok(response)
                }
                Err(service_error) => {
                    let boxed_error: BoxError = service_error.into();

                    // Convert the error to GraphQL format using the HeapErrorToGraphQL trait
                    let graphql_error = boxed_error.to_graphql_error();

                    // Create GraphQL response structure
                    let graphql_response = serde_json::json!({
                        "data": null,
                        "errors": [graphql_error],
                        "extensions": {}
                    });

                    let response_stream: ResponseStream =
                        Box::pin(stream::once(async move { graphql_response }));

                    Ok(JsonResponse {
                        extensions: Default::default(),
                        responses: response_stream,
                    })
                }
            }
        })
    }
}

#[cfg(test)]
mod tests;
