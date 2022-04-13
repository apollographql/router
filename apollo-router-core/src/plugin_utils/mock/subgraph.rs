use crate::{plugin_utils, Request, Response, SubgraphRequest, SubgraphResponse};
use futures::future;
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
        let builder = plugin_utils::SubgraphResponse::builder().context(req.context);
        let response = if let Some(response) = self.mocks.get(req.http_request.body()) {
            builder.data(response.data.clone()).build().into()
        } else {
            panic!(
                "could not find mock for query: {}",
                serde_json::to_string(req.http_request.body())
                    .expect("could not serialise request")
            );
        };
        future::ok(response)
    }
}
