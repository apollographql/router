use crate::{RouterResponse, SubgraphRequest};
use apollo_router_core::prelude::*;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use tower::{BoxError, Service};
use typed_builder::TypedBuilder;
use url::Url;

#[derive(TypedBuilder, Clone)]
pub struct ReqwestSubgraphService {
    http_client: reqwest_middleware::ClientWithMiddleware,
    // TODO not used because provided by SubgraphRequest
    service: Arc<String>,
    // TODO not used because provided by SubgraphRequest
    url: Arc<Url>,
}

impl ReqwestSubgraphService {
    /// Construct a new http subgraph fetcher that will fetch from the supplied URL.
    pub fn new(service: impl Into<String>, url: Url) -> Self {
        let service = service.into();

        Self {
            http_client: reqwest_middleware::ClientBuilder::new(
                reqwest::Client::builder()
                    .tcp_keepalive(Some(std::time::Duration::from_secs(5)))
                    .build()
                    .unwrap(),
            )
            .with(reqwest_tracing::TracingMiddleware)
            .with(LoggingMiddleware::new(&service))
            .build(),
            service: Arc::new(service),
            url: Arc::new(url),
        }
    }
}

impl Service<SubgraphRequest> for ReqwestSubgraphService {
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

struct LoggingMiddleware {
    service: String,
}

impl LoggingMiddleware {
    fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }
}

#[async_trait::async_trait]
impl reqwest_middleware::Middleware for LoggingMiddleware {
    async fn handle(
        &self,
        req: reqwest::Request,
        extensions: &mut task_local_extensions::Extensions,
        next: reqwest_middleware::Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        tracing::trace!("Request to service {}: {:?}", self.service, req);
        let res = next.run(req, extensions).await;
        tracing::trace!("Response from service {}: {:?}", self.service, res);
        res
    }
}
