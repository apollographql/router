use apollo_router_core::prelude::*;
use bytes::Bytes;
use futures::prelude::*;
use std::pin::Pin;

type BytesStream = Pin<
    Box<dyn futures::Stream<Item = Result<bytes::Bytes, graphql::FetchError>> + std::marker::Send>,
>;

/// A fetcher for subgraph data that uses http.
/// Streaming via chunking is supported.
#[derive(Debug)]
pub struct HttpSubgraphFetcher {
    service: String,
    url: String,
    http_client: reqwest::Client,
}

impl HttpSubgraphFetcher {
    /// Construct a new http subgraph fetcher that will fetch from the supplied URL.
    pub fn new(service: String, url: String) -> Self {
        HttpSubgraphFetcher {
            service,
            url,
            http_client: reqwest::Client::builder()
                .tcp_keepalive(Some(std::time::Duration::from_secs(5)))
                .build()
                .unwrap(),
        }
    }

    fn request_stream(&self, request: graphql::Request) -> BytesStream {
        // Perform the actual request and start streaming.
        // Reqwest doesn't care if there is only one response, in this case it'll be a stream of one element.
        let service = self.service.to_owned();
        self.http_client
            .post(self.url.clone())
            .json(&request)
            .send()
            // We have a future for the response, convert it to a future of the stream.
            .map_ok(|r| r.bytes_stream().boxed())
            // Convert the entire future to a stream, at this point we have a stream of a result of a single stream
            .into_stream()
            // Flatten the stream
            .flat_map(|result| match result {
                Ok(s) => s,
                Err(err) => stream::iter(vec![Err(err)]).boxed(),
            })
            .map_err(
                move |err: reqwest::Error| graphql::FetchError::SubrequestHttpError {
                    service: service.to_owned(),
                    reason: err.to_string(),
                },
            )
            .boxed()
    }

    fn map_to_graphql(&self, bytes_stream: BytesStream) -> graphql::ResponseStream {
        // Map the stream of bytes to our response type.
        let service = self.service.to_owned();
        let response_stream = bytes_stream
            // We want to know if this is the primary response or not, so attach the response count
            .enumerate()
            .map(move |(index, result)| {
                let service = service.to_owned();
                match result {
                    Ok(bytes) => to_response(service, &bytes, index == 0),
                    Err(err) => err.to_response(index == 0),
                }
            });
        response_stream.boxed()
    }
}

fn to_response(service: impl Into<String>, bytes: &Bytes, primary: bool) -> graphql::Response {
    serde_json::from_slice::<graphql::Response>(bytes)
        .map_err(
            move |err| graphql::FetchError::SubrequestMalformedResponse {
                service: service.into(),
                reason: err.to_string(),
            },
        )
        .unwrap_or_else(|err| err.to_response(primary))
}

impl graphql::Fetcher for HttpSubgraphFetcher {
    /// Using reqwest fetch a stream of graphql results.
    fn stream(&self, request: graphql::Request) -> graphql::ResponseStream {
        let bytes_stream = self.request_stream(request);
        self.map_to_graphql(bytes_stream)
    }
}

#[cfg(test)]
mod tests {
    use httpmock::Method::POST;
    use httpmock::{MockServer, Regex};
    use serde_json::json;

    use super::*;

    #[tokio::test]
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
        let fetcher = HttpSubgraphFetcher::new("products".into(), server.url("/graphql"));
        let collect = fetcher
            .stream(
                graphql::Request::builder()
                    .query(r#"{allProducts{variation {id}id}}"#)
                    .build(),
            )
            .collect::<Vec<_>>()
            .await;

        assert_eq!(collect[0], response);
        mock.assert();
        Ok(())
    }
}
