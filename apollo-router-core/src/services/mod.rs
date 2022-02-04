mod execution_service;
mod router_service;

pub use self::execution_service::*;
pub use self::router_service::*;
use crate::header_manipulation::HeaderManipulationLayer;
use crate::layers::cache::CachingLayer;
use crate::prelude::graphql::*;
use http::header::{HeaderName, COOKIE};
use http::HeaderValue;
use moka::sync::Cache;
use static_assertions::assert_impl_all;
use std::hash::Hash;
use std::str::FromStr;
use std::sync::Arc;
use tower::layer::util::Stack;
use tower::ServiceBuilder;
use tower_service::Service;

// the parsed graphql Request, HTTP headers and contextual data for extensions
pub struct RouterRequest {
    pub http_request: http::Request<Request>,

    // Context for extension
    pub context: Context<()>,
}

impl From<http::Request<Request>> for RouterRequest {
    fn from(http_request: http::Request<Request>) -> Self {
        Self {
            http_request,
            context: Context::new(),
        }
    }
}

assert_impl_all!(PlannedRequest: Send);
/// TODO confusing name since this is a Response
pub struct PlannedRequest {
    pub query_plan: Arc<QueryPlan>,

    pub context: Context,
}

assert_impl_all!(SubgraphRequest: Send);
pub struct SubgraphRequest {
    pub http_request: http::Request<Request>,

    pub context: Context,
}

assert_impl_all!(QueryPlannerRequest: Send);
pub struct QueryPlannerRequest {
    pub options: QueryPlanOptions,

    pub context: Context,
}

assert_impl_all!(RouterResponse: Send);
pub struct RouterResponse {
    pub response: http::Response<Response>,

    pub context: Context,
}

impl AsRef<Request> for http::Request<Request> {
    fn as_ref(&self) -> &Request {
        self.body()
    }
}

impl AsRef<Request> for Arc<http::Request<Request>> {
    fn as_ref(&self) -> &Request {
        self.body()
    }
}

pub trait ServiceBuilderExt<L> {
    //This will only compile for Endpoint services
    fn propagate_all_headers(self) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>>;
    fn propagate_header(
        self,
        header_name: &str,
    ) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>>;
    fn propagate_or_default_header(
        self,
        header_name: &str,
        value: HeaderValue,
    ) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>>;
    fn remove_header(self, header_name: &str) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>>;
    fn insert_header(
        self,
        header_name: &str,
        value: HeaderValue,
    ) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>>;
    fn propagate_cookies(self) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>>;
    fn cache<S, Request, Key, Value, KeyFn, ValueFn, ResponseFn>(
        self,
        cache: Cache<Key, Value>,
        key_fn: KeyFn,
        value_fn: ValueFn,
        response_fn: ResponseFn,
    ) -> ServiceBuilder<Stack<CachingLayer<S, Request, Key, Value, KeyFn, ValueFn, ResponseFn>, L>>
    where
        Request: Send,
        S: Service<Request> + Send,
        Key: Send + Sync + Eq + Hash + Clone + 'static,
        Value: Send + Sync + Clone + 'static,
        KeyFn: Fn(&Request) -> Key + Clone + Send + 'static,
        ValueFn: Fn(&S::Response) -> Value + Clone + Send + 'static,
        ResponseFn: Fn(Request, Value) -> S::Response + Clone + Send + 'static,
        <S as Service<Request>>::Error: Send + Sync,
        <S as Service<Request>>::Response: Send,
        <S as Service<Request>>::Future: Send;
}

//Demonstrate adding reusable stuff to ServiceBuilder.
impl<L> ServiceBuilderExt<L> for ServiceBuilder<L> {
    fn propagate_all_headers(
        self: ServiceBuilder<L>,
    ) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>> {
        self.layer(HeaderManipulationLayer::propagate_all())
    }

    fn propagate_header(
        self: ServiceBuilder<L>,
        header_name: &str,
    ) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>> {
        self.layer(HeaderManipulationLayer::propagate(
            HeaderName::from_str(header_name).unwrap(),
        ))
    }

    fn propagate_or_default_header(
        self: ServiceBuilder<L>,
        header_name: &str,
        default_header_value: HeaderValue,
    ) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>> {
        self.layer(HeaderManipulationLayer::propagate_or_default(
            HeaderName::from_str(header_name).unwrap(),
            default_header_value,
        ))
    }

    fn insert_header(
        self: ServiceBuilder<L>,
        header_name: &str,
        header_value: HeaderValue,
    ) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>> {
        self.layer(HeaderManipulationLayer::insert(
            HeaderName::from_str(header_name).unwrap(),
            header_value,
        ))
    }

    fn remove_header(
        self: ServiceBuilder<L>,
        header_name: &str,
    ) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>> {
        self.layer(HeaderManipulationLayer::remove(
            HeaderName::from_str(header_name).unwrap(),
        ))
    }

    fn propagate_cookies(self) -> ServiceBuilder<Stack<HeaderManipulationLayer, L>> {
        self.layer(HeaderManipulationLayer::propagate(COOKIE))
    }

    fn cache<S, Request, Key, Value, KeyFn, ValueFn, ResponseFn>(
        self,
        cache: Cache<Key, Value>,
        key_fn: KeyFn,
        value_fn: ValueFn,
        response_fn: ResponseFn,
    ) -> ServiceBuilder<Stack<CachingLayer<S, Request, Key, Value, KeyFn, ValueFn, ResponseFn>, L>>
    where
        Request: Send,
        S: Service<Request> + Send,
        Key: Send + Sync + Eq + Hash + Clone + 'static,
        Value: Send + Sync + Clone + 'static,
        KeyFn: Fn(&Request) -> Key + Clone + Send + 'static,
        ValueFn: Fn(&S::Response) -> Value + Clone + Send + 'static,
        ResponseFn: Fn(Request, Value) -> S::Response + Clone + Send + 'static,
        <S as Service<Request>>::Error: Send + Sync,
        <S as Service<Request>>::Response: Send,
        <S as Service<Request>>::Future: Send,
    {
        self.layer(CachingLayer::new(cache, key_fn, value_fn, response_fn))
    }
}
