pub use self::checkpoint::{AsyncCheckpointLayer, CheckpointLayer};
pub use self::execution_service::*;
pub use self::router_service::*;
use crate::fetch::OperationKind;
use crate::layers::cache::CachingLayer;
use crate::prelude::graphql::*;
use futures::future::BoxFuture;
use http::StatusCode;
use http::{method::Method, request::Builder, Uri};
use moka::sync::Cache;
use serde::{Deserialize, Serialize};
use serde_json_bytes::ByteString;
use static_assertions::assert_impl_all;
use std::convert::Infallible;
use std::ops::ControlFlow;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower::buffer::BufferLayer;
use tower::layer::util::Stack;
use tower::{BoxError, ServiceBuilder};
use tower_service::Service;

pub mod checkpoint;
mod execution_service;
pub mod http_compat;
mod router_service;
mod tower_subgraph_service;
pub use tower_subgraph_service::TowerSubgraphService;

pub(crate) const DEFAULT_BUFFER_SIZE: usize = 20_000;

impl From<http_compat::Request<Request>> for RouterRequest {
    fn from(http_request: http_compat::Request<Request>) -> Self {
        Self {
            originating_request: Arc::new(http_request),
            // context: Context::new().with_request(http_request),
            context: Context::new(),
        }
    }
}

/// Different kinds of body we could have as the Router's response
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
#[serde(untagged)]
pub enum ResponseBody {
    /// A GraphQL response corresponding to the spec <https://spec.graphql.org/October2021/#sec-Response>
    GraphQL(Response),
    /// A json value
    RawJSON(serde_json::Value),
    /// Text without any serialization (example: HTML content, Prometheus metrics, ...)
    Text(String),
}

impl TryFrom<ResponseBody> for Response {
    type Error = &'static str;

    fn try_from(value: ResponseBody) -> Result<Self, Self::Error> {
        match value {
            ResponseBody::GraphQL(res) => Ok(res),
            ResponseBody::RawJSON(_) => {
                Err("wrong ResponseBody kind: expected Response, found RawJSON")
            }
            ResponseBody::Text(_) => Err("wrong ResponseBody kind: expected Response, found Text"),
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
            ResponseBody::Text(res) => Ok(res),
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
            ResponseBody::Text(_) => Err("wrong ResponseBody kind: expected RawJSON, found Text"),
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
        Ok(Self::Text(s.to_owned()))
    }
}

assert_impl_all!(RouterRequest: Send);
/// [`Context`] for the request.
///
/// This consists of the parsed graphql Request, HTTP headers and contextual data for extensions.
pub struct RouterRequest {
    /// Original request to the Router.
    pub originating_request: Arc<http_compat::Request<Request>>,

    // Context for extension
    pub context: Context,
}

/*
query: Option<String>,
operation_name: Option<String>,
variables: Option<Arc<Object>>,
#[builder(default, setter(!strip_option, transform = |extensions: Vec<(&str, Value)>| Some(from_names_and_values(extensions))))]
extensions: Option<Object>,
context: Option<Context<http_compat::Request<crate::Request>>>,
headers: Option<Vec<(String, String)>>,
*/
// #[buildstructor::builder]
impl RouterRequest {
    pub fn new(
        query: Option<String>,
        operation_name: Option<String>,
        variables: Arc<Object>,
        input_extensions: Vec<(&'static str, Value)>,
        context: Context,
        headers: Vec<(String, String)>,
    ) -> RouterRequest {
        let extensions: Object = input_extensions
            .into_iter()
            .map(|(name, value)| (ByteString::from(name.to_string()), value))
            .collect();
        let gql_request = crate::Request {
            query,
            operation_name,
            variables,
            extensions,
        };

        /*
        let mut req = RequestBuilder::new(Method::GET, Uri::from_str("http://default").unwrap());

        for (key, value) in headers {
            req = req.header(key, value);
        }
        let req = req.body(gql_request).expect("body is always valid; qed");
        */
        let mut builder = Builder::new()
            .method(Method::GET)
            .uri(Uri::from_str("http://default").unwrap());
        for (key, value) in headers {
            builder = builder.header(key, value);
        }
        let req = builder.body(gql_request).expect("body is always valid qed");

        let req = http_compat::Request { inner: req };
        Self {
            originating_request: Arc::new(req),
            context,
        }
    }
}

assert_impl_all!(RouterResponse: Send);
/// [`Context`] and [`http_compat::Response<ResponseBody>`] for the response.
///
/// This consists of the response body and the context.
#[derive(Clone)]
pub struct RouterResponse {
    pub response: http_compat::Response<ResponseBody>,
    pub context: Context,
}

// #[buildstructor::builder]
impl RouterResponse {
    pub fn new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<crate::Error>,
        extensions: Object,
        status_code: Option<StatusCode>,
        context: Context,
    ) -> RouterResponse {
        // Build a response
        let res = Response::builder()
            .label(label)
            .data(data.unwrap_or_default())
            .path(path)
            .errors(errors)
            .extensions(extensions)
            .build();

        // Build an http Response
        let http_response = http::Response::builder()
            .status(status_code.unwrap_or(StatusCode::OK))
            .body(ResponseBody::GraphQL(res))
            .expect("ResponseBody is serializable; qed");

        // Create a compatible Response
        let compat_response = http_compat::Response {
            inner: http_response,
        };

        Self {
            response: compat_response,
            context,
        }
    }
}

assert_impl_all!(QueryPlannerRequest: Send);
/// [`Context`] for the request.
pub struct QueryPlannerRequest {
    /// Original request to the Router.
    pub originating_request: Arc<http_compat::Request<Request>>,

    pub context: Context,
}

// #[buildstructor::builder]
impl QueryPlannerRequest {
    pub fn new(
        originating_request: Arc<http_compat::Request<Request>>,
        context: Context,
    ) -> QueryPlannerRequest {
        Self {
            originating_request,
            context,
        }
    }
}

assert_impl_all!(QueryPlannerResponse: Send);
/// [`Context`] and [`QueryPlan`] for the response..
pub struct QueryPlannerResponse {
    pub query_plan: Arc<QueryPlan>,

    pub context: Context,
}

// #[buildstructor::builder]
impl QueryPlannerResponse {
    pub fn new(query_plan: Arc<QueryPlan>, context: Context) -> QueryPlannerResponse {
        Self {
            query_plan,
            context,
        }
    }
}

assert_impl_all!(SubgraphRequest: Send);
/// [`Context`], [`OperationKind`] and [`http_compat::Request<Request>`] for the request.
pub struct SubgraphRequest {
    /// Original request to the Router.
    pub originating_request: Arc<http_compat::Request<Request>>,

    pub http_request: http_compat::Request<Request>,

    pub operation_kind: OperationKind,

    pub context: Context,
}

// #[buildstructor::builder]
impl SubgraphRequest {
    pub fn new(
        originating_request: Arc<http_compat::Request<Request>>,
        http_request: http_compat::Request<Request>,
        operation_kind: OperationKind,
        context: Context,
    ) -> SubgraphRequest {
        Self {
            originating_request,
            http_request,
            operation_kind,
            context,
        }
    }
}

assert_impl_all!(SubgraphResponse: Send);
/// [`Context`] and [`http_compat::Response<Response>`] for the response.
///
/// This consists of the subgraph response and the context.
#[derive(Clone, Debug)]
pub struct SubgraphResponse {
    pub response: http_compat::Response<Response>,

    pub context: Context,
}

// #[buildstructor::builder]
impl SubgraphResponse {
    pub fn new(response: http_compat::Response<Response>, context: Context) -> SubgraphResponse {
        Self { response, context }
    }

    pub fn new_from_bits(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<crate::Error>,
        extensions: Object,
        status_code: Option<StatusCode>,
        context: Context,
    ) -> SubgraphResponse {
        // Build a response
        let res = Response::builder()
            .label(label)
            .data(data.unwrap_or_default())
            .path(path)
            .errors(errors)
            .extensions(extensions)
            .build();

        // Build an http Response
        let http_response = http::Response::builder()
            .status(status_code.unwrap_or(StatusCode::OK))
            .body(res)
            .expect("Response is serializable; qed");

        // Create a compatible Response
        let compat_response = http_compat::Response {
            inner: http_response,
        };

        Self {
            response: compat_response,
            context,
        }
    }
}

assert_impl_all!(ExecutionRequest: Send);
/// [`Context`] and [`QueryPlan`] for the request.
pub struct ExecutionRequest {
    /// Original request to the Router.
    pub originating_request: Arc<http_compat::Request<Request>>,

    pub query_plan: Arc<QueryPlan>,

    pub context: Context,
}

// #[buildstructor::builder]
impl ExecutionRequest {
    pub fn new(
        originating_request: Arc<http_compat::Request<Request>>,
        query_plan: Arc<QueryPlan>,
        context: Context,
    ) -> ExecutionRequest {
        Self {
            originating_request,
            query_plan,
            context,
        }
    }
}

assert_impl_all!(ExecutionResponse: Send);
/// [`Context`] and [`http_compat::Response<Response>`] for the response.
///
/// This consists of the execution response and the context.
pub struct ExecutionResponse {
    pub response: http_compat::Response<Response>,

    pub context: Context,
}

// #[buildstructor::builder]
impl ExecutionResponse {
    pub fn new(response: http_compat::Response<Response>, context: Context) -> ExecutionResponse {
        Self { response, context }
    }

    pub fn new_from_bits(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<crate::Error>,
        extensions: Object,
        status_code: Option<StatusCode>,
        context: Context,
    ) -> ExecutionResponse {
        // Build a response
        let res = Response::builder()
            .label(label)
            .data(data.unwrap_or_default())
            .path(path)
            .errors(errors)
            .extensions(extensions)
            .build();

        // Build an http Response
        let http_response = http::Response::builder()
            .status(status_code.unwrap_or(StatusCode::OK))
            .body(res)
            .expect("Response is serializable; qed");

        // Create a compatible Response
        let compat_response = http_compat::Response {
            inner: http_response,
        };

        Self {
            response: compat_response,
            context,
        }
    }
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

/// Extension to the [`ServiceBuilder`] trait to make it easy to add router specific capabilities
/// (e.g.: checkpoints) to a [`Service`].
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
    fn buffered<Request>(self) -> ServiceBuilder<Stack<BufferLayer<Request>, L>>;
    fn layer<T>(self, layer: T) -> ServiceBuilder<Stack<T, L>>;
}

#[allow(clippy::type_complexity)]
impl<L> ServiceBuilderExt<L> for ServiceBuilder<L> {
    fn layer<T>(self, layer: T) -> ServiceBuilder<Stack<T, L>> {
        ServiceBuilder::layer(self, layer)
    }

    fn buffered<Request>(self) -> ServiceBuilder<Stack<BufferLayer<Request>, L>> {
        self.buffer(DEFAULT_BUFFER_SIZE)
    }
}
