use apollo_router_core::prelude::*;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use typed_builder::TypedBuilder;

#[derive(TypedBuilder, Clone)]
pub struct ReqwestSubgraphService {
    http_client: reqwest_middleware::ClientWithMiddleware,
    service: Arc<String>,
    // TODO not used because provided by SubgraphRequest
    url: Arc<reqwest::Url>,
}

impl ReqwestSubgraphService {
    /// Construct a new http subgraph fetcher that will fetch from the supplied URL.
    pub fn new(service: impl Into<String>, url: reqwest::Url) -> Self {
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

impl tower::Service<graphql::SubgraphRequest> for ReqwestSubgraphService {
    type Response = graphql::RouterResponse;
    type Error = tower::BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        //TODO backpressure
        Poll::Ready(Ok(()))
    }

    fn call(
        &mut self,
        graphql::SubgraphRequest {
            http_request,
            context,
        }: graphql::SubgraphRequest,
    ) -> Self::Future {
        let http_client = self.http_client.clone();
        Box::pin(async move {
            tracing::debug!(
                "Making request to {} {:?}",
                http_request.uri(),
                http_request,
            );
            let (
                http::request::Parts {
                    method,
                    uri,
                    version,
                    headers,
                    extensions: _,
                    ..
                },
                body,
            ) = http_request.into_parts();
            let mut request =
                reqwest::Request::new(method, reqwest::Url::parse(&uri.to_string()).expect("todo"));
            *request.headers_mut() = headers;
            *request.version_mut() = version;
            let response = http_client.execute(request).await?;

            let graphql: graphql::Response = serde_json::from_slice(&response.bytes().await?)?;
            Ok(graphql::RouterResponse {
                response: http::Response::builder().body(graphql).expect("no argument can fail to parse or converted to the internal representation here; qed"),
                context,
            })
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
