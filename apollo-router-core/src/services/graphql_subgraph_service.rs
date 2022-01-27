use crate::prelude::graphql;
use crate::{RouterResponse, SubgraphRequest};
use std::future::Future;
use std::pin::Pin;
use std::task::Poll;
use tower::{BoxError, Service};
use typed_builder::TypedBuilder;

#[derive(TypedBuilder, Clone)]
pub struct GraphqlSubgraphService {
    http_client: reqwest_middleware::ClientWithMiddleware,
}

impl Service<SubgraphRequest> for GraphqlSubgraphService {
    type Response = RouterResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        //TODO backpressure
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: SubgraphRequest) -> Self::Future {
        let http_client = self.http_client.clone();
        Box::pin(async move {
            tracing::debug!(
                "Making request to {} {:?}",
                request.http_request.uri(),
                request.http_request
            );
            let response = http_client
                .post(request.http_request.uri().to_string())
                .body(reqwest::Body::from(serde_json::to_vec(
                    request.http_request.body(),
                )?))
                .headers(request.http_request.headers().to_owned())
                .send()
                .await;

            match response {
                Ok(resp) => {
                    let graphql: graphql::Response = serde_json::from_slice(&resp.bytes().await?)?;
                    Ok(RouterResponse {
                        response: http::Response::builder().body(graphql)?,
                        context: request.context,
                    })
                }
                Err(e) => Err(BoxError::from(e)),
            }
        })
    }
}
