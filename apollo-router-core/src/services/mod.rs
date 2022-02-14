mod execution_service;
pub mod http_compat;
mod router_service;

pub use self::execution_service::*;
pub use self::router_service::*;
use crate::header_manipulation::HeaderManipulationLayer;
use crate::layers::cache::CachingLayer;
use crate::prelude::graphql::*;
use http::header::{HeaderName, COOKIE};
use http::HeaderValue;
use moka::sync::Cache;
use serde::{Deserialize, Serialize};
use static_assertions::assert_impl_all;
use std::convert::Infallible;
use std::str::FromStr;
use std::sync::Arc;
use tower::layer::util::Stack;
use tower::ServiceBuilder;
use tower_service::Service;

impl From<http_compat::Request<Request>> for RouterRequest {
    fn from(http_request: http_compat::Request<Request>) -> Self {
        Self {
            context: Context::new().with_request(http_request),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub enum ResponseBody {
    GraphQL(Response),
    RawJSON(serde_json::Value),
    RawString(String),
}

impl From<Response> for ResponseBody {
    fn from(response: Response) -> Self {
        Self::GraphQL(response)
    }
}

impl From<serde_json::Value> for ResponseBody {
    fn from(json: serde_json::Value) -> Self {
        Self::RawJSON(json)
    }
}

// This impl is purposefully done this way to hint users this might not be what they would like to do.
/// Creates a ResponseBody from a &str
///
/// /!\ No serialization or deserialization is involved,
/// please make sure you don't want to send a GraphQL response or a Raw JSON payload instead.
impl FromStr for ResponseBody {
    type Err = Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::RawString(s.to_owned()))
    }
}

assert_impl_all!(RouterRequest: Send);
// the parsed graphql Request, HTTP headers and contextual data for extensions
pub struct RouterRequest {
    // Context for extension
    pub context: Context<http_compat::Request<Request>>,
}

assert_impl_all!(RouterResponse: Send);
pub struct RouterResponse {
    pub response: http_compat::Response<ResponseBody>,
    pub context: Context,
}

assert_impl_all!(QueryPlannerRequest: Send);
pub struct QueryPlannerRequest {
    pub context: Context,
}

assert_impl_all!(QueryPlannerResponse: Send);
pub struct QueryPlannerResponse {
    pub query_plan: Arc<QueryPlan>,

    pub context: Context,
}

assert_impl_all!(SubgraphRequest: Send);
pub struct SubgraphRequest {
    pub http_request: http_compat::Request<Request>,

    pub context: Context,

    pub operation_kind: OperationKind,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum OperationKind {
    Query,
    Mutation,
    Subscription,
}

assert_impl_all!(SubgraphResponse: Send);
#[derive(Clone, Debug)]
pub struct SubgraphResponse {
    pub response: http_compat::Response<Response>,

    pub context: Context,
}

assert_impl_all!(ExecutionRequest: Send);
pub struct ExecutionRequest {
    pub query_plan: Arc<QueryPlan>,

    pub context: Context,
}

assert_impl_all!(ExecutionResponse: Send);
pub struct ExecutionResponse {
    pub response: http_compat::Response<Response>,

    pub context: Context,
}

impl AsRef<Request> for http_compat::Request<Request> {
    fn as_ref(&self) -> &Request {
        self.body()
    }
}

impl AsRef<Request> for Arc<http_compat::Request<Request>> {
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

    #[allow(clippy::type_complexity)]
    fn cache<S, Request, Key, Value, KeyFn, ValueFn, ResponseFn>(
        self,
        cache: Cache<Key, Result<Value, S::Error>>,
        key_fn: KeyFn,
        value_fn: ValueFn,
        response_fn: ResponseFn,
    ) -> ServiceBuilder<Stack<CachingLayer<S, Request, Key, Value, KeyFn, ValueFn, ResponseFn>, L>>
    where
        Request: Send,
        S: Service<Request> + Send,
        <S as Service<Request>>::Error: Send + Sync + Clone,
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

    #[allow(clippy::type_complexity)]
    fn cache<S, Request, Key, Value, KeyFn, ValueFn, ResponseFn>(
        self,
        cache: Cache<Key, Result<Value, S::Error>>,
        key_fn: KeyFn,
        value_fn: ValueFn,
        response_fn: ResponseFn,
    ) -> ServiceBuilder<Stack<CachingLayer<S, Request, Key, Value, KeyFn, ValueFn, ResponseFn>, L>>
    where
        Request: Send,
        S: Service<Request> + Send,
        <S as Service<Request>>::Error: Send + Sync + Clone,
        <S as Service<Request>>::Response: Send,
        <S as Service<Request>>::Future: Send,
    {
        self.layer(CachingLayer::new(cache, key_fn, value_fn, response_fn))
    }
}
