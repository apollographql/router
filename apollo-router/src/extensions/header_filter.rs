use apollo_router_core::prelude::*;
use hyper::HeaderMap;
use std::{
    collections::HashSet,
    task::{Context, Poll},
};

use tower::{Layer, Service};

pub struct HeaderFilterLayer {
    allowed_headers: HashSet<String>,
}

impl HeaderFilterLayer {
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

impl<S> Service<graphql::HttpRequest> for HeaderFilter<S>
where
    S: Service<graphql::HttpRequest, Error = ()>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, mut req: graphql::HttpRequest) -> Self::Future {
        let mut current_header = None;
        let mut filtered_headers = HeaderMap::new();

        for (name, value) in req.headers.into_iter() {
            if let Some(name) = name {
                if self.allowed_headers.contains(name.as_str()) {
                    current_header = Some(name.clone());
                } else {
                    current_header = None;
                }
            }

            if let Some(ref name) = current_header {
                filtered_headers.insert(name.clone(), value);
            }
        }

        req.headers = filtered_headers;

        self.service.call(req)
    }
}
