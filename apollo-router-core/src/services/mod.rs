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
use tower::{BoxError, ServiceBuilder};
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

#[derive(Serialize, Deserialize, Clone)]
#[serde(untagged)]
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
#[derive(Clone)]
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

#[allow(clippy::type_complexity)]
pub trait ServiceBuilderExt<L> {
    fn cache<S, Request, Key, Value>(
        self,
        cache: Cache<Key, Result<Value, String>>,
        key_fn: fn(&Request) -> Key,
        value_fn: fn(&S::Response) -> Value,
        response_fn: fn(Request, Value) -> S::Response,
    ) -> ServiceBuilder<Stack<CachingLayer<S, Request, Key, Value>, L>>
    where
        Request: Send,
        S: Service<Request> + Send,
        <S as Service<Request>>::Error: Into<BoxError> + Send + Sync,
        <S as Service<Request>>::Response: Send,
        <S as Service<Request>>::Future: Send;

    fn cache_query_plan<S>(
        self,
    ) -> ServiceBuilder<
        Stack<
            CachingLayer<S, QueryPlannerRequest, (Option<String>, Option<String>), Arc<QueryPlan>>,
            L,
        >,
    >
    where
        S: Service<QueryPlannerRequest, Response = QueryPlannerResponse> + Send,
        <S as Service<QueryPlannerRequest>>::Error: Into<BoxError> + Send + Sync,
        <S as Service<QueryPlannerRequest>>::Response: Send,
        <S as Service<QueryPlannerRequest>>::Future: Send;
}

#[allow(clippy::type_complexity)]
impl<L> ServiceBuilderExt<L> for ServiceBuilder<L> {
    fn cache<S, Request, Key, Value>(
        self,
        cache: Cache<Key, Result<Value, String>>,
        key_fn: fn(&Request) -> Key,
        value_fn: fn(&S::Response) -> Value,
        response_fn: fn(Request, Value) -> S::Response,
    ) -> ServiceBuilder<Stack<CachingLayer<S, Request, Key, Value>, L>>
    where
        Request: Send,
        S: Service<Request> + Send,
        <S as Service<Request>>::Error: Into<BoxError> + Send + Sync,
        <S as Service<Request>>::Response: Send,
        <S as Service<Request>>::Future: Send,
    {
        self.layer(CachingLayer::new(cache, key_fn, value_fn, response_fn))
    }

    fn cache_query_plan<S>(
        self,
    ) -> ServiceBuilder<
        Stack<
            CachingLayer<S, QueryPlannerRequest, (Option<String>, Option<String>), Arc<QueryPlan>>,
            L,
        >,
    >
    where
        S: Service<QueryPlannerRequest, Response = QueryPlannerResponse> + Send,
        <S as Service<QueryPlannerRequest>>::Error: Into<BoxError> + Send + Sync,
        <S as Service<QueryPlannerRequest>>::Response: Send,
        <S as Service<QueryPlannerRequest>>::Future: Send,
    {
        let plan_cache_limit = std::env::var("ROUTER_PLAN_CACHE_LIMIT")
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(100);
        self.cache(
            moka::sync::CacheBuilder::new(plan_cache_limit).build(),
            |r: &QueryPlannerRequest| {
                (
                    r.context.request.body().query.clone(),
                    r.context.request.body().operation_name.clone(),
                )
            },
            |r: &QueryPlannerResponse| r.query_plan.clone(),
            |r: QueryPlannerRequest, v: Arc<QueryPlan>| QueryPlannerResponse {
                query_plan: v,
                context: r.context,
            },
        )
    }
}
