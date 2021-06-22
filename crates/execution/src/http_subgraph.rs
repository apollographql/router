use futures::{FutureExt, StreamExt, TryFutureExt, TryStreamExt};

use crate::{
    FetchError, GraphQLFetcher, GraphQLPatchResponse, GraphQLPrimaryResponse, GraphQLRequest,
    GraphQLResponse, GraphQLResponseStream,
};

/// A fetcher for subgraph data that uses http.
/// Streaming via chunking is supported.
#[derive(Debug)]
pub struct HttpSubgraphFetcher {
    url: String,
    reqwest: reqwest::Client,
}

impl HttpSubgraphFetcher {
    /// Construct a new http subgraph fetcher that will fetch from the supplied URL.
    pub fn new(url: String) -> HttpSubgraphFetcher {
        HttpSubgraphFetcher {
            url,
            reqwest: reqwest::Client::new(),
        }
    }
}

impl GraphQLFetcher for HttpSubgraphFetcher {
    /// Using reqwest fetch a stream of graphql results.
    fn stream(&self, request: &GraphQLRequest) -> GraphQLResponseStream {
        // Perform the actual request and start streaming.
        // Reqwest doesn't care if there is only one response, in this case it'll be a stream of one element.
        let bytes_stream = self
            .reqwest
            .post(self.url.clone())
            .json(request)
            .send()
            // We have a future for the response, convert it to a future of the stream.
            .map_ok(|r| r.bytes_stream().boxed())
            // Convert the entire future to a stream, at this point we have a stream of a single stream
            .into_stream()
            // Flatten the stream
            .flat_map(|result| match result {
                Ok(s) => s,
                Err(err) => futures::stream::iter(vec![Err(err)]).boxed(),
            });

        // Map the stream of bytes to our response type.
        let response_stream = bytes_stream
            .map_err(FetchError::from)
            // We want to know if this is the primary response or not, so attach the response count
            .enumerate()
            .map(|e| match e.1 {
                Ok(bytes) => Ok((e.0, bytes)),
                Err(err) => Err(err),
            })
            // If the index is zero then it is a primary response, if it's non-zero it's a secondary response.
            .and_then(|(index, bytes)| {
                if index == 0 {
                    futures::future::ready(serde_json::from_slice::<GraphQLPrimaryResponse>(&bytes))
                        .map_err(FetchError::from)
                        .map_ok(GraphQLResponse::Primary)
                        .boxed()
                } else {
                    futures::future::ready(serde_json::from_slice::<GraphQLPatchResponse>(&bytes))
                        .map_err(FetchError::from)
                        .map_ok(GraphQLResponse::Patch)
                        .boxed()
                }
            })
            .boxed();
        response_stream
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_non_chunked() -> Result<(), Box<dyn std::error::Error>> {
        let fetcher = HttpSubgraphFetcher::new("http://localhost:4001/graphql".to_string());
        let collect = fetcher
            .stream(&GraphQLRequest {
                query: r#"{allProducts{variation {id}id}}"#.into(),
                extensions: None,
                operation_name: None,
                variables: None,
            })
            .collect::<Vec<Result<GraphQLResponse, FetchError>>>()
            .await;

        assert_eq!(
            collect[0].as_ref().unwrap(),
            &GraphQLResponse::Primary(GraphQLPrimaryResponse {
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
                extensions: None
            })
        );
        Ok(())
    }
}
