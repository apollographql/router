use apollo_router_core::prelude::*;
use futures::lock::Mutex;
use futures::{future::BoxFuture, TryFutureExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;
use tokio::sync::broadcast::{self, Sender};
use tracing::Instrument;
use typed_builder::TypedBuilder;

#[derive(TypedBuilder, Clone)]
pub struct ReqwestSubgraphService {
    http_client: reqwest_middleware::ClientWithMiddleware,
    service: Arc<String>,
    // TODO not used because provided by SubgraphRequest
    // FIXME: debatable because here we would end up reparsing the URL on every call
    // which would be a performance regression. The SubgraphRequest type should provide
    // a url::Url instead of using the http crate
    // for now, to make things work, if the URL in the request is /, we use this URL
    url: reqwest::Url,

    wait_map: Arc<
        Mutex<
            HashMap<
                apollo_router_core::http_compat::Request<graphql::Request>,
                Sender<
                    Result<
                        apollo_router_core::http_compat::Response<graphql::Response>,
                        graphql::FetchError,
                    >,
                >,
            >,
        >,
    >,
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
            url,
            wait_map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn dedup(
        &self,
        graphql::SubgraphRequest {
            http_request,
            context,
        }: graphql::SubgraphRequest,
    ) -> Result<graphql::SubgraphResponse, graphql::FetchError> {
        loop {
            let mut locked_wait_map = self.wait_map.lock().await;
            match locked_wait_map.get_mut(&http_request) {
                Some(waiter) => {
                    // Register interest in key
                    let mut receiver = waiter.subscribe();
                    drop(locked_wait_map);

                    match receiver.recv().await {
                        Ok(value) => {
                            return value
                                .map(|response| graphql::SubgraphResponse { response, context })
                        }
                        // there was an issue with the broadcast channel, retry fetching
                        Err(_) => continue,
                    }
                }
                None => {
                    let (tx, _rx) = broadcast::channel(1);
                    locked_wait_map.insert(http_request.clone(), tx.clone());
                    drop(locked_wait_map);

                    let res = self.fetch(http_request.clone()).await;

                    {
                        let mut locked_wait_map = self.wait_map.lock().await;
                        locked_wait_map.remove(&http_request);
                    }

                    // Let our waiters know
                    let broadcast_value = res.clone();
                    // Our use case is very specific, so we are sure that
                    // we won't get any errors here.
                    tokio::task::spawn_blocking(move || {
                    tx.send(broadcast_value)
                        .expect("there is always at least one receiver alive, the _rx guard; qed")
                }).await
                .expect("can only fail if the task is aborted or if the internal code panics, neither is possible here; qed");
                    return res.map(|response| graphql::SubgraphResponse { response, context });
                }
            }
        }
    }

    async fn fetch(
        &self,
        http_request: apollo_router_core::http_compat::Request<apollo_router_core::Request>,
    ) -> Result<
        apollo_router_core::http_compat::Response<apollo_router_core::Response>,
        graphql::FetchError,
    > {
        let http_client = self.http_client.clone();
        let target_url = if http_request.uri() == "/" {
            self.url.clone()
        } else {
            reqwest::Url::parse(&http_request.uri().to_string()).expect("todo")
        };
        let service_name = (*self.service).to_owned();

        let (
            http::request::Parts {
                method,
                version,
                headers,
                extensions: _,
                ..
            },
            body,
        ) = http_request.into_parts();

        let mut request = http_client
            .request(method, target_url)
            .json(&body)
            .build()
            .map_err(|err| {
                tracing::error!(fetch_error = err.to_string().as_str());
                graphql::FetchError::SubrequestHttpError {
                    service: (*self.service).to_owned(),
                    reason: err.to_string(),
                }
            })?;
        request.headers_mut().extend(headers.into_iter());
        *request.version_mut() = version;

        let response = http_client.execute(request).await.map_err(|err| {
            tracing::error!(fetch_error = err.to_string().as_str());
            graphql::FetchError::SubrequestHttpError {
                service: (*self.service).to_owned(),
                reason: err.to_string(),
            }
        })?;
        let body = response
            .bytes()
            .instrument(tracing::debug_span!("aggregate_response_data"))
            .await
            .map_err(|err| {
                tracing::error!(fetch_error = format!("{:?}", err).as_str());

                graphql::FetchError::SubrequestHttpError {
                    service: service_name.clone(),
                    reason: err.to_string(),
                }
            })?;

        let graphql: graphql::Response =
            tracing::debug_span!("parse_subgraph_response").in_scope(|| {
                graphql::Response::from_bytes(&service_name, body).map_err(|error| {
                    graphql::FetchError::SubrequestMalformedResponse {
                        service: service_name.clone(),
                        reason: error.to_string(),
                    }
                })
            })?;

        Ok(http::Response::builder().body(graphql).expect("no argument can fail to parse or converted to the internal representation here; qed").into())
    }
}

impl tower::Service<graphql::SubgraphRequest> for ReqwestSubgraphService {
    type Response = graphql::SubgraphResponse;
    type Error = tower::BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        //TODO backpressure
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: graphql::SubgraphRequest) -> Self::Future {
        Box::pin(self.dedup(request).map_err(|e| e.into()))
        /*let http_client = self.http_client.clone();
        let target_url = if http_request.uri() == "/" {
            self.url.clone()
        } else {
            reqwest::Url::parse(&http_request.uri().to_string()).expect("todo")
        };
        let service_name = (*self.service).to_owned();

        Box::pin(async move {
            let (
                http::request::Parts {
                    method,
                    version,
                    headers,
                    extensions: _,
                    ..
                },
                body,
            ) = http_request.into_parts();

            let mut request = http_client
                .request(method, target_url)
                .json(&body)
                .build()?;
            request.headers_mut().extend(headers.into_iter());
            *request.version_mut() = version;

            let response = http_client.execute(request).await?;
            let body = response
                .bytes()
                .instrument(tracing::debug_span!("aggregate_response_data"))
                .await
                .map_err(|err| {
                    tracing::error!(fetch_error = format!("{:?}", err).as_str());

                    graphql::FetchError::SubrequestHttpError {
                        service: service_name.clone(),
                        reason: err.to_string(),
                    }
                })?;

            let graphql: graphql::Response = tracing::debug_span!("parse_subgraph_response")
                .in_scope(|| {
                    graphql::Response::from_bytes(&service_name, body).map_err(|error| {
                        graphql::FetchError::SubrequestMalformedResponse {
                            service: service_name.clone(),
                            reason: error.to_string(),
                        }
                    })
                })?;

            Ok(graphql::SubgraphResponse {
                response: http::Response::builder().body(graphql).expect("no argument can fail to parse or converted to the internal representation here; qed").into(),
                context,
            })
        })*/
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
