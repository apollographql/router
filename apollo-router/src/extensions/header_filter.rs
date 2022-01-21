use apollo_router_core::prelude::*;
use std::{
    collections::HashSet,
    task::{Context, Poll},
};

use tower::{Layer, Service};

pub struct HeaderFilterLayer {
    allowed_headers: HashSet<String>,
}

impl HeaderFilterLayer {
    #[allow(dead_code)]
    pub fn new<I>(allowed_headers: I) -> Self
    where
        I: Iterator<Item = String>,
    {
        HeaderFilterLayer {
            allowed_headers: allowed_headers.map(|s| s.to_lowercase()).collect(),
        }
    }
}

impl<S> Layer<S> for HeaderFilterLayer {
    type Service = HeaderFilter<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HeaderFilter {
            service: inner,
            allowed_headers: self.allowed_headers.clone(),
        }
    }
}

pub struct HeaderFilter<S> {
    service: S,
    allowed_headers: HashSet<String>,
}

impl<S> Service<http::Request<graphql::Request>> for HeaderFilter<S>
where
    S: Service<http::Request<graphql::Request>, Error = ()>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, mut req: http::Request<graphql::Request>) -> Self::Future {
        let removed_keys: Vec<_> = req
            .headers()
            .keys()
            .filter(|name| !self.allowed_headers.contains(name.as_str()))
            .cloned()
            .collect();

        let headers_mut = req.headers_mut();
        for name in removed_keys {
            headers_mut.remove(name);
        }

        self.service.call(req)
    }
}
