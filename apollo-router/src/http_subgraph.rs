use apollo_router_core::prelude::*;
use async_trait::async_trait;
use derivative::Derivative;
use tracing::Instrument;
use url::Url;

/// A fetcher for subgraph data that uses http.
/// Streaming via chunking is supported.
#[derive(Derivative)]
#[derivative(Debug, Clone)]
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

    #[allow(dead_code)]
    fn create_request(
        &self,
        request: http::Request<graphql::Request>,
    ) -> reqwest_middleware::RequestBuilder {
        let (parts, request) = request.into_parts();
        let mut header_map = reqwest::header::HeaderMap::new();
        header_map.extend(parts.headers.into_iter());

        self.http_client.post(self.url.clone()).json(&request)
    }

    async fn send_request(
        service: &str,
        req: reqwest_middleware::RequestBuilder,
    ) -> Result<bytes::Bytes, graphql::FetchError> {
        let response = req
            .send()
            .instrument(tracing::trace_span!("http-subgraph-request"))
            .await
            .map_err(|err| {
                tracing::error!(fetch_error = err.to_string().as_str());

                graphql::FetchError::SubrequestHttpError {
                    service: service.to_owned(),
                    reason: err.to_string(),
                }
            })?;

        response
            .bytes()
            .instrument(tracing::debug_span!("aggregate_response_data"))
            .await
            .map_err(|err| {
                tracing::error!(fetch_error = format!("{:?}", err).as_str());

                graphql::FetchError::SubrequestHttpError {
                    service: service.to_owned(),
                    reason: err.to_string(),
                }
            })
    }

    async fn request_stream(
        &self,
        request: &graphql::Request,
    ) -> Result<bytes::Bytes, graphql::FetchError> {
        //let req = self.create_request(request);
        let req = self.http_client.post(self.url.clone()).json(&request);
        Self::send_request(&self.service, req).await
    }

    fn map_to_graphql(
        service_name: String,
        response: bytes::Bytes,
    ) -> Result<graphql::Response, graphql::FetchError> {
        tracing::debug_span!("parse_subgraph_response").in_scope(|| {
            graphql::Response::from_bytes(&service_name, response).map_err(|error| {
                graphql::FetchError::SubrequestMalformedResponse {
                    service: service_name.clone(),
                    reason: error.to_string(),
                }
            })
        })
    }
}

#[async_trait]
impl graphql::Fetcher for HttpSubgraphFetcher {
    /// Using reqwest to fetch a graphql response
    async fn stream(
        &self,
        request: &graphql::Request,
    ) -> Result<graphql::Response, graphql::FetchError> {
        let service_name = self.service.to_string();
        let response = self.request_stream(request).await?;
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
        let expected_response = graphql::Response::builder()
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
                .json_body_obj(&expected_response);
        });
        let fetcher =
            HttpSubgraphFetcher::new("products", Url::parse(&server.url("/graphql")).unwrap());
        let response = fetcher
            .stream(
                &graphql::Request::builder()
                    .query(r#"{allProducts{variation {id}id}}"#)
                    .build(),
            )
            .await
            .unwrap();

        assert_eq!(response, expected_response);
        mock.assert();
        Ok(())
    }
}
