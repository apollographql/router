//! Mock subgraph implementation

use crate::{Object, Request, Response, SubgraphRequest, SubgraphResponse};
use futures::future;
use http::StatusCode;
use std::{collections::HashMap, sync::Arc, task::Poll};
use tower::{BoxError, Service};

type MockResponses = HashMap<Request, Response>;

#[derive(Clone, Default)]
pub struct MockSubgraph {
    // using an arc to improve efficiency when service is cloned
    mocks: Arc<MockResponses>,
}

impl MockSubgraph {
    pub fn new(mocks: MockResponses) -> Self {
        Self {
            mocks: Arc::new(mocks),
        }
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
        // let builder = utils::SubgraphResponse::builder().context(req.context);
        let response = if let Some(response) = self.mocks.get(req.subgraph_request.body()) {
            // Build an http Response
            let http_response = http::Response::builder()
                .status(StatusCode::OK)
                .body(response.clone())
                .expect("Response is serializable; qed");

            // Create a compatible Response
            let compat_response = crate::http_compat::Response {
                inner: http_response,
            };

            SubgraphResponse::new_with_response(compat_response, req.context)
        } else {
            let errors = vec![crate::Error {
                message: "couldn't find mock for query".to_string(),
                locations: Default::default(),
                path: Default::default(),
                extensions: Default::default(),
            }];
            SubgraphResponse::builder()
                .errors(errors)
                .extensions(Object::new())
                .context(req.context)
                .build()
        };
        future::ok(response)
    }
}
