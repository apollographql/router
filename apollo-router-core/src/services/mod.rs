pub use self::checkpoint::{AsyncCheckpointLayer, CheckpointLayer};
pub use self::execution_service::*;
pub use self::router_service::*;
use crate::fetch::OperationKind;
use crate::layers::cache::CachingLayer;
use crate::prelude::graphql::*;
use futures::future::BoxFuture;
use moka::sync::Cache;
use serde::{Deserialize, Serialize};
use static_assertions::assert_impl_all;
use std::convert::Infallible;
use std::ops::ControlFlow;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower::layer::util::Stack;
use tower::{BoxError, ServiceBuilder};
use tower_service::Service;

pub mod checkpoint;
mod execution_service;
pub mod http_compat;
mod reqwest_subgraph_service;
mod router_service;
pub use reqwest_subgraph_service::ReqwestSubgraphService;

impl From<http_compat::Request<Request>> for RouterRequest {
    fn from(http_request: http_compat::Request<Request>) -> Self {
        Self {
            context: Context::new().with_request(http_request),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(untagged)]
pub enum ResponseBody {
    GraphQL(Response),
    RawJSON(serde_json::Value),
    RawString(String),
}

impl TryFrom<ResponseBody> for Response {
    type Error = &'static str;

    fn try_from(value: ResponseBody) -> Result<Self, Self::Error> {
        match value {
            ResponseBody::GraphQL(res) => Ok(res),
            ResponseBody::RawJSON(_) => {
                Err("wrong ResponseBody kind: expected Response, found RawJSON")
            }
            ResponseBody::RawString(_) => {
                Err("wrong ResponseBody kind: expected Response, found RawString")
            }
        }
    }
}

impl TryFrom<ResponseBody> for String {
    type Error = &'static str;

    fn try_from(value: ResponseBody) -> Result<Self, Self::Error> {
        match value {
            ResponseBody::RawJSON(_) => {
                Err("wrong ResponseBody kind: expected RawString, found RawJSON")
            }
            ResponseBody::GraphQL(_) => {
                Err("wrong ResponseBody kind: expected RawString, found GraphQL")
            }
            ResponseBody::RawString(res) => Ok(res),
        }
    }
}

impl TryFrom<ResponseBody> for serde_json::Value {
    type Error = &'static str;

    fn try_from(value: ResponseBody) -> Result<Self, Self::Error> {
        match value {
            ResponseBody::RawJSON(res) => Ok(res),
            ResponseBody::GraphQL(_) => {
                Err("wrong ResponseBody kind: expected RawJSON, found GraphQL")
            }
            ResponseBody::RawString(_) => {
                Err("wrong ResponseBody kind: expected RawJSON, found RawString")
            }
        }
    }
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

#[allow(clippy::type_complexity)]
pub trait ServiceBuilderExt<L>: Sized {
    fn cache<Request, Response, Key, Value>(
        self,
        cache: Cache<Key, Arc<RwLock<Option<Result<Value, String>>>>>,
        key_fn: fn(&Request) -> Option<&Key>,
        value_fn: fn(&Response) -> &Value,
        response_fn: fn(Request, Value) -> Response,
    ) -> ServiceBuilder<Stack<CachingLayer<Request, Response, Key, Value>, L>>
    where
        Request: Send,
    {
        self.layer(CachingLayer::new(cache, key_fn, value_fn, response_fn))
    }

    fn checkpoint<S, Request>(
        self,
        checkpoint_fn: impl Fn(
                Request,
            ) -> Result<
                ControlFlow<<S as Service<Request>>::Response, Request>,
                <S as Service<Request>>::Error,
            > + Send
            + Sync
            + 'static,
    ) -> ServiceBuilder<Stack<CheckpointLayer<S, Request>, L>>
    where
        S: Service<Request> + Send + 'static,
        Request: Send + 'static,
        S::Future: Send,
        S::Response: Send + 'static,
        S::Error: Into<BoxError> + Send + 'static,
    {
        self.layer(CheckpointLayer::new(checkpoint_fn))
    }

    fn async_checkpoint<S, Request>(
        self,
        async_checkpoint_fn: impl Fn(
                Request,
            ) -> BoxFuture<
                'static,
                Result<ControlFlow<<S as Service<Request>>::Response, Request>, BoxError>,
            > + Send
            + Sync
            + 'static,
    ) -> ServiceBuilder<Stack<AsyncCheckpointLayer<S, Request>, L>>
    where
        S: Service<Request, Error = BoxError> + Clone + Send + 'static,
        Request: Send + 'static,
        S::Future: Send,
        S::Response: Send + 'static,
    {
        self.layer(AsyncCheckpointLayer::new(async_checkpoint_fn))
    }
    fn layer<T>(self, layer: T) -> ServiceBuilder<Stack<T, L>>;
}

#[allow(clippy::type_complexity)]
impl<L> ServiceBuilderExt<L> for ServiceBuilder<L> {
    fn layer<T>(self, layer: T) -> ServiceBuilder<Stack<T, L>> {
        ServiceBuilder::layer(self, layer)
    }
}
