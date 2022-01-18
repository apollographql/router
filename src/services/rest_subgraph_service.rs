use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

use http::{Response, Uri};
use tower::{BoxError, Service};
use tracing::info;
use typed_builder::TypedBuilder;

use crate::{graphql, RouterResponse, SubgraphRequest};

#[derive(TypedBuilder)]
pub struct RestSubgraphService {
    #[builder(setter(into))]
    url: Uri,
}

impl Service<SubgraphRequest> for RestSubgraphService {
    type Response = RouterResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        //TODO backpressure
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: SubgraphRequest) -> Self::Future {
        let url = request.url_override.unwrap_or(self.url.clone());
        let fut = async move {
            info!("Making request to {} {:?}", url, request.subgraph_request);
            Ok(RouterResponse {
                request: request.request,
                response: Response::new(graphql::Response {
                    body: format!(
                        "{} World from {}",
                        request.subgraph_request.body().body,
                        url
                    ),
                }),
                context: request.context,
            })
        };

        // Return the response as an immediate future
        Box::pin(fut)
    }
}
