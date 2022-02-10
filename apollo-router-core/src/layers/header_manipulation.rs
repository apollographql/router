use crate::layer::ConfigurableLayer;
use crate::{register_layer, SubgraphRequest};
use http::header::HeaderName;
use http::HeaderValue;
use std::task::Poll;
use tower::{Layer, Service};

#[derive(Clone)]
pub enum Operation {
    PropagateAll,
    Propagate(HeaderName),
    PropagateOrDefault(HeaderName, HeaderValue),
    Insert(HeaderName, HeaderValue),
    Remove(HeaderName),
}

register_layer!("headers", HeaderManipulationLayer);
impl ConfigurableLayer for HeaderManipulationLayer {
    type Config = ();
}

#[derive(Default)]
pub struct HeaderManipulationLayer {
    operations: Vec<Operation>,
}

impl HeaderManipulationLayer {
    pub fn new(operations: Vec<Operation>) -> Self {
        Self { operations }
    }
}

impl From<Operation> for HeaderManipulationLayer {
    fn from(operation: Operation) -> Self {
        HeaderManipulationLayer::new(vec![operation])
    }
}

impl<S> Layer<S> for HeaderManipulationLayer {
    type Service = HeaderManipulationService<S>;

    fn layer(&self, service: S) -> Self::Service {
        HeaderManipulationService {
            service,
            operations: self.operations.to_owned(),
        }
    }
}

pub struct HeaderManipulationService<S> {
    service: S,
    operations: Vec<Operation>,
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
        {
            //Add the header to the request and pass it on to the service.
            let subgraph_request_headers = request.http_request.headers_mut();

            for operation in &self.operations {
                match operation {
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
                            subgraph_request_headers
                                .insert(header_name.to_owned(), default_value.clone());
                        }
                    }
                    Operation::Insert(header_name, header_value) => {
                        subgraph_request_headers
                            .insert(header_name.to_owned(), header_value.clone());
                    }
                    Operation::Remove(header_name) => {
                        subgraph_request_headers.remove(header_name);
                    }
                }
            }
        }

        self.service.call(request)
    }
}
