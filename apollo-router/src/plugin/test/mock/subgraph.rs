//! Mock subgraph implementation

use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;

use futures::future;
use http::StatusCode;
use tower::BoxError;
use tower::Service;

use crate::graphql::Request;
use crate::graphql::Response;
use crate::json_ext::Object;
use crate::SubgraphRequest;
use crate::SubgraphResponse;

type MockResponses = HashMap<Request, Response>;

#[derive(Clone, Default)]
pub struct MockSubgraph {
    // using an arc to improve efficiency when service is cloned
    mocks: Arc<MockResponses>,
    extensions: Option<Object>,
}

impl MockSubgraph {
    pub fn new(mocks: MockResponses) -> Self {
        Self {
            mocks: Arc::new(mocks),
            extensions: None,
        }
    }

    pub fn with_extensions(mut self, extensions: Object) -> Self {
        self.extensions = Some(extensions);
        self
    }
}

impl Service<SubgraphRequest> for MockSubgraph {
    type Response = SubgraphResponse;

    type Error = BoxError;

    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: SubgraphRequest) -> Self::Future {
        let response = if let Some(response) = self.mocks.get(req.subgraph_request.body()) {
            // Build an http Response
            let http_response = http::Response::builder()
                .status(StatusCode::OK)
                .body(response.clone())
                .expect("Response is serializable; qed");

            // Create a compatible Response
            let compat_response = crate::http_ext::Response {
                inner: http_response,
            };

            SubgraphResponse::new_from_response(compat_response, req.context)
        } else {
            let error = crate::error::Error::builder()
                .message("couldn't find mock for query".to_string())
                .extensions(self.extensions.clone().unwrap_or_default())
                .build();
            SubgraphResponse::fake_builder()
                .error(error)
                .context(req.context)
                .build()
        };
        future::ok(response)
    }
}
