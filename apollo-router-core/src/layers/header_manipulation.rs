use std::task::Poll;

use crate::layers::header_manipulation::Operation::{
    Insert, Propagate, PropagateAll, PropagateOrDefault, Remove,
};
use http::header::HeaderName;
use http::HeaderValue;
use tower::{Layer, Service};

use crate::SubgraphRequest;

#[derive(Clone)]
pub enum Operation {
    PropagateAll,
    Propagate(HeaderName),
    PropagateOrDefault(HeaderName, HeaderValue),
    Insert(HeaderName, HeaderValue),
    Remove(HeaderName),
}

pub struct HeaderManipulationLayer {
    operation: Operation,
}

impl HeaderManipulationLayer {
    pub(crate) fn propagate_all() -> HeaderManipulationLayer {
        Self {
            operation: PropagateAll,
        }
    }

    pub(crate) fn propagate(header_name: HeaderName) -> HeaderManipulationLayer {
        Self {
            operation: Propagate(header_name),
        }
    }

    pub(crate) fn propagate_or_default(
        header_name: HeaderName,
        header_value: HeaderValue,
    ) -> HeaderManipulationLayer {
        Self {
            operation: PropagateOrDefault(header_name, header_value),
        }
    }

    pub(crate) fn insert(
        header_name: HeaderName,
        header_value: HeaderValue,
    ) -> HeaderManipulationLayer {
        Self {
            operation: Insert(header_name, header_value),
        }
    }

    pub(crate) fn remove(header_name: HeaderName) -> HeaderManipulationLayer {
        Self {
            operation: Remove(header_name),
        }
    }
}

impl<S> Layer<S> for HeaderManipulationLayer {
    type Service = HeaderManipulationService<S>;

    fn layer(&self, service: S) -> Self::Service {
        HeaderManipulationService {
            service,
            operation: self.operation.to_owned(),
        }
    }
}

pub struct HeaderManipulationService<S> {
    service: S,
    operation: Operation,
}

impl<S> Service<SubgraphRequest> for HeaderManipulationService<S>
where
    S: Service<SubgraphRequest>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, mut request: SubgraphRequest) -> Self::Future {
        //Add the header to the request and pass it on to the service.
        let subgraph_request_headers = request.http_request.headers_mut();
        match &self.operation {
            Operation::PropagateAll => {
                for (header_name, header_value) in request.context.request.headers() {
                    subgraph_request_headers.insert(header_name, header_value.clone());
                }
            }
            Operation::Propagate(header_name) => {
                if let Some(header) = request.context.request.headers().get(header_name) {
                    subgraph_request_headers.insert(header_name.to_owned(), header.clone());
                }
            }
            Operation::PropagateOrDefault(header_name, default_value) => {
                if let Some(header) = request.context.request.headers().get(header_name) {
                    subgraph_request_headers.insert(header_name.to_owned(), header.clone());
                } else {
                    subgraph_request_headers.insert(header_name.to_owned(), default_value.clone());
                }
            }
            Operation::Insert(header_name, header_value) => {
                subgraph_request_headers.insert(header_name.to_owned(), header_value.clone());
            }
            Operation::Remove(header_name) => {
                subgraph_request_headers.remove(header_name);
            }
        }

        self.service.call(request)
    }
}
