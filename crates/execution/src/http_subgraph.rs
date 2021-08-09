use std::pin::Pin;

use bytes::Bytes;
use futures::prelude::*;

use crate::{
    FetchError, GraphQLFetcher, GraphQLPatchResponse, GraphQLPrimaryResponse, GraphQLRequest,
    GraphQLResponse, GraphQLResponseStream,
};

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
    pub fn new(service: String, url: String) -> Self {
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
            // Convert the entire future to a stream, at this point we have a stream of a result of a single stream
            .into_stream()
            // Flatten the stream
            .flat_map(|result| match result {
                Ok(s) => s,
                Err(err) => stream::iter(vec![Err(err)]).boxed(),
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
            .map(move |(index, result)| {
                let service = service.to_owned();
                match result {
                    Ok(bytes) if { index == 0 } => to_primary(service, &bytes),
                    Ok(bytes) => to_patch(service, &bytes),
                    Err(err) => Err(err),
                }
            });
        response_stream.boxed()
    }
}

fn to_patch(service: String, bytes: &Bytes) -> Result<GraphQLResponse, FetchError> {
    serde_json::from_slice::<GraphQLPatchResponse>(&bytes)
        .map(GraphQLResponse::Patch)
        .map_err(move |err| FetchError::ServiceError {
            service,
            reason: err.to_string(),
        })
}

fn to_primary(service: String, bytes: &Bytes) -> Result<GraphQLResponse, FetchError> {
    serde_json::from_slice::<GraphQLPrimaryResponse>(&bytes)
        .map(GraphQLResponse::Primary)
        .map_err(move |err| FetchError::ServiceError {
            service,
            reason: err.to_string(),
        })
}

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
            has_next: Default::default(),
            errors: Default::default(),
            extensions: Default::default(),
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
            .stream(
                GraphQLRequest::builder()
                    .query(r#"{allProducts{variation {id}id}}"#)
                    .build(),
            )
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
