use apollo_router_core::prelude::*;
use async_trait::async_trait;
use derivative::Derivative;
use futures::prelude::*;
use tracing::Instrument;
use url::Url;

/// A fetcher for subgraph data that uses http.
/// Streaming via chunking is supported.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct HttpSubgraphFetcher {
    service: String,
    url: Url,
    #[derivative(Debug = "ignore")]
    http_client: reqwest_middleware::ClientWithMiddleware,
}

impl HttpSubgraphFetcher {
    /// Construct a new http subgraph fetcher that will fetch from the supplied URL.
    pub fn new(service: impl Into<String>, url: Url) -> Self {
        let service = service.into();

        HttpSubgraphFetcher {
            http_client: reqwest_middleware::ClientBuilder::new(
                reqwest::Client::builder()
                    .tcp_keepalive(Some(std::time::Duration::from_secs(5)))
                    .build()
                    .unwrap(),
            )
            .with(reqwest_tracing::TracingMiddleware)
            .with(LoggingMiddleware::new(&service))
            .build(),
            service,
            url,
        }
    }

    async fn request_stream(
        &self,
        request: graphql::Request,
    ) -> Result<bytes::Bytes, graphql::FetchError> {
        // Perform the actual request and start streaming.
        // assume for now that there will be only one response
        let response = self
            .http_client
            .post(self.url.clone())
            .json(&request)
            .send()
            .instrument(tracing::trace_span!("http-subgraph-request"))
            .await
            .map_err(|err| {
                tracing::error!(fetch_error = format!("{:?}", err).as_str());

                graphql::FetchError::SubrequestHttpError {
                    service: self.service.to_owned(),
                    reason: err.to_string(),
                }
            })?;

        response.bytes().await.map_err(|err| {
            tracing::error!(fetch_error = format!("{:?}", err).as_str());

            graphql::FetchError::SubrequestHttpError {
                service: self.service.to_owned(),
                reason: err.to_string(),
            }
        })
    }

    fn map_to_graphql(
        service_name: String,
        response: Result<bytes::Bytes, graphql::FetchError>,
    ) -> graphql::ResponseStream {
        Box::pin(
            async move {
                let is_primary = true;
                match response {
                    Err(e) => e.to_response(is_primary),
                    Ok(bytes) => serde_json::from_slice::<graphql::Response>(&bytes)
                        .unwrap_or_else(|error| {
                            graphql::FetchError::SubrequestMalformedResponse {
                                service: service_name.clone(),
                                reason: error.to_string(),
                            }
                            .to_response(is_primary)
                        }),
                }
            }
            .into_stream(),
        )
    }
}

#[async_trait]
impl graphql::Fetcher for HttpSubgraphFetcher {
    /// Using reqwest fetch a stream of graphql results.
    async fn stream(&self, request: graphql::Request) -> graphql::ResponseStream {
        let service_name = self.service.to_string();
        let response = self.request_stream(request).await;
        Self::map_to_graphql(service_name, response)
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

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::POST;
    use httpmock::{MockServer, Regex};
    use serde_json::json;
    use test_log::test;

    #[test(tokio::test)]
    async fn test_non_chunked() -> Result<(), Box<dyn std::error::Error>> {
        let response = graphql::Response::builder()
            .data(json!({
              "allProducts": [
                {
                  "variation": {
                    "id": "OSS"
                  },
                  "id": "apollo-federation"
                },
                {
                  "variation": {
                    "id": "platform"
                  },
                  "id": "apollo-studio"
                }
              ]
            }))
            .build();

        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_matches(Regex::new(".*").unwrap());
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body_obj(&response);
        });
        let fetcher =
            HttpSubgraphFetcher::new("products", Url::parse(&server.url("/graphql")).unwrap());
        let collect = fetcher
            .stream(
                graphql::Request::builder()
                    .query(r#"{allProducts{variation {id}id}}"#)
                    .build(),
            )
            .await
            .collect::<Vec<_>>()
            .await;

        assert_eq!(collect[0], response);
        mock.assert();
        Ok(())
    }
}
