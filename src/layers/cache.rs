use std::task::Poll;

use tower::{Layer, Service};

#[derive(Debug, Clone)]
pub struct CacheLayer;

pub struct Cache<S> {
    service: S,
}

impl<S, Request> Service<Request> for Cache<S>
where
    S: Service<Request>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        self.service.call(req)
    }
}

impl<S> Layer<S> for CacheLayer {
    type Service = Cache<S>;

    fn layer(&self, service: S) -> Self::Service {
        Cache { service }
    }
}
