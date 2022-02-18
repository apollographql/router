pub use self::execution_service::*;
pub use self::router_service::*;
use crate::fetch::OperationKind;
use crate::layers::cache::CachingLayer;
use crate::prelude::graphql::*;
use moka::sync::Cache;
use serde::{Deserialize, Serialize};
use static_assertions::assert_impl_all;
use std::convert::Infallible;
use std::str::FromStr;
use std::sync::Arc;
use tower::layer::util::Stack;
use tower::ServiceBuilder;
use tower_service::Service;
mod execution_service;
pub mod http_compat;
mod router_service;

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
