use std::pin::Pin;

use futures::{FutureExt, StreamExt, TryFutureExt, TryStreamExt};

use crate::{
    FetchError, GraphQLFetcher, GraphQLPatchResponse, GraphQLPrimaryResponse, GraphQLRequest,
    GraphQLResponse, GraphQLResponseStream,
};
use futures::stream::iter;

type BytesStream =
    Pin<Box<dyn futures::Stream<Item = Result<bytes::Bytes, FetchError>> + std::marker::Send>>;

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
    pub fn new(service: String, url: String) -> HttpSubgraphFetcher {
        HttpSubgraphFetcher {
            service,
            url,
            http_client: reqwest::Client::new(),
        }
    }

    fn request_stream(&self, request: GraphQLRequest) -> BytesStream {
        // Perform the actual request and start streaming.
        // Reqwest doesn't care if there is only one response, in this case it'll be a stream of one element.
        let service = self.service.to_owned();
        self.http_client
            .post(self.url.clone())
            .json(&request)
            .send()
            // We have a future for the response, convert it to a future of the stream.
            .map_ok(|r| r.bytes_stream().boxed())
            // Convert the entire future to a stream, at this point we have a stream of a single stream
            .into_stream()
            // Flatten the stream
            .flat_map(|result| match result {
                Ok(s) => s,
                Err(err) => iter(vec![Err(err)]).boxed(),
            })
            .map_err(move |err| FetchError::ServiceError {
                service: service.to_owned(),
                reason: err.to_string(),
            })
            .boxed()
    }

    fn map_to_graphql(&self, bytes_stream: BytesStream) -> GraphQLResponseStream {
        // Map the stream of bytes to our response type.
        let service = self.service.to_owned();
        let response_stream = bytes_stream
            // We want to know if this is the primary response or not, so attach the response count
            .enumerate()
            .map(|e| match e.1 {
                Ok(bytes) => Ok((e.0, bytes)),
                Err(err) => Err(err),
            })
            // If the index is zero then it is a primary response, if it's non-zero it's a secondary response.
            .and_then(move |(index, bytes)| {
                let service = service.to_owned();
                if index == 0 {
                    futures::future::ready(serde_json::from_slice::<GraphQLPrimaryResponse>(&bytes))
                        .map_err(move |err| FetchError::ServiceError {
                            service,
                            reason: err.to_string(),
                        })
                        .map_ok(GraphQLResponse::Primary)
                        .boxed()
                } else {
                    futures::future::ready(serde_json::from_slice::<GraphQLPatchResponse>(&bytes))
                        .map_err(move |err| FetchError::ServiceError {
                            service,
                            reason: err.to_string(),
                        })
                        .map_ok(GraphQLResponse::Patch)
                        .boxed()
                }
            });
        response_stream.boxed()
    }
}

trait MapToGraphQL {}

impl GraphQLFetcher for HttpSubgraphFetcher {
    /// Using reqwest fetch a stream of graphql results.
    fn stream(&self, request: GraphQLRequest) -> GraphQLResponseStream {
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
        let response = GraphQLPrimaryResponse {
            data: json!({
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
            })
            .as_object()
            .unwrap()
            .to_owned(),
            has_next: None,
            errors: None,
            extensions: None,
        };

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
            .stream(GraphQLRequest {
                query: r#"{allProducts{variation {id}id}}"#.into(),
                extensions: None,
                operation_name: None,
                variables: None,
            })
            .collect::<Vec<Result<GraphQLResponse, FetchError>>>()
            .await;

        assert_eq!(
            collect[0].as_ref().unwrap(),
            &GraphQLResponse::Primary(response)
        );
        mock.assert();
        Ok(())
    }
}
