use crate::SubgraphRequest;
use http::header::HeaderName;

use std::task::Poll;
use tower::{Layer, Service};

pub struct PropagateHeaderLayer {
    header_name: HeaderName,
}

impl PropagateHeaderLayer {
    pub(crate) fn new(header_name: HeaderName) -> PropagateHeaderLayer {
        Self { header_name }
    }
}

impl<S> Layer<S> for PropagateHeaderLayer {
    type Service = PropagateHeaderService<S>;

    fn layer(&self, service: S) -> Self::Service {
        PropagateHeaderService {
            service,
            header_name: self.header_name.to_owned(),
        }
    }
}

pub struct PropagateHeaderService<S> {
    service: S,
    header_name: HeaderName,
}

impl<S> Service<SubgraphRequest> for PropagateHeaderService<S>
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
        if let Some(header) = request.request.headers().get(&self.header_name) {
            request
                .subgraph_request
                .headers_mut()
                .insert(self.header_name.to_owned(), header.clone());
        }
        self.service.call(request)
    }
}
