//! Implementation of the various steps in the router's processing pipeline.

pub use self::checkpoint::{AsyncCheckpointLayer, CheckpointLayer};
pub use self::execution_service::*;
pub use self::router_service::*;
use crate::fetch::OperationKind;
use crate::layers::cache::CachingLayer;
use crate::prelude::graphql::*;
use futures::future::BoxFuture;
use http::{header::HeaderName, HeaderValue, StatusCode};
use http::{method::Method, Uri};
use http_compat::IntoHeaderName;
use http_compat::IntoHeaderValue;
use moka::sync::Cache;
use multimap::MultiMap;
use serde::{Deserialize, Serialize};
use serde_json_bytes::ByteString;
use static_assertions::assert_impl_all;
use std::collections::HashMap;
use std::convert::Infallible;
use std::ops::ControlFlow;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower::buffer::BufferLayer;
use tower::layer::util::Stack;
use tower::{BoxError, ServiceBuilder};
use tower_service::Service;
use tracing::Span;

pub mod checkpoint;
mod execution_service;
pub mod http_compat;
mod router_service;
mod tower_subgraph_service;
use crate::instrument::InstrumentLayer;
pub use tower_subgraph_service::TowerSubgraphService;

pub const DEFAULT_BUFFER_SIZE: usize = 20_000;

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
/// Represents the router processing step of the processing pipeline.
///
/// This consists of the parsed graphql Request, HTTP headers and contextual data for extensions.
pub struct RouterRequest {
    /// Original request to the Router.
    pub originating_request: http_compat::Request<Request>,

    /// Context for extension
    pub context: Context,
}

impl From<http_compat::Request<Request>> for RouterRequest {
    fn from(originating_request: http_compat::Request<Request>) -> Self {
        Self {
            originating_request,
            context: Context::new(),
        }
    }
}

#[buildstructor::builder]
impl RouterRequest {
    /// This is the constructor (or builder) to use when constructing a real RouterRequest.
    ///
    /// Required parameters are required in non-testing code to create a RouterRequest.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        query: Option<String>,
        operation_name: Option<String>,
        variables: HashMap<String, Value>,
        extensions: HashMap<String, Value>,
        context: Context,
        headers: MultiMap<IntoHeaderName, IntoHeaderValue>,
        uri: Uri,
        method: Method,
    ) -> Result<RouterRequest, BoxError> {
        let extensions: Object = extensions
            .into_iter()
            .map(|(name, value)| (ByteString::from(name), value))
            .collect();

        let variables: Object = variables
            .into_iter()
            .map(|(name, value)| (ByteString::from(name), value))
            .collect();

        let gql_request = Request::builder()
            .query(query)
            .operation_name(operation_name)
            .variables(variables)
            .extensions(extensions)
            .build();

        let originating_request = http_compat::Request::builder()
            .headers(headers)
            .uri(uri)
            .method(method)
            .body(gql_request)
            .build()?;

        Ok(Self {
            originating_request,
            context,
        })
    }

    /// This is the constructor (or builder) to use when constructing a "fake" RouterRequest.
    ///
    /// This does not enforce the provision of the data that is required for a fully functional
    /// RouterRequest. It's usually enough for testing, when a fully constructed RouterRequest is
    /// difficult to construct and not required for the purposes of the test.
    ///
    /// In addition, fake requests are expected to be valid, and will panic if given invalid values.
    pub fn fake_new(
        query: Option<String>,
        operation_name: Option<String>,
        variables: HashMap<String, Value>,
        extensions: HashMap<String, Value>,
        context: Option<Context>,
        headers: MultiMap<IntoHeaderName, IntoHeaderValue>,
    ) -> Result<RouterRequest, BoxError> {
        RouterRequest::new(
            query,
            operation_name,
            variables,
            extensions,
            context.unwrap_or_default(),
            headers,
            Uri::from_static("http://default"),
            Method::GET,
        )
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

#[buildstructor::builder]
impl RouterResponse {
    /// This is the constructor (or builder) to use when constructing a real RouterResponse..
    ///
    /// Required parameters are required in non-testing code to create a RouterResponse..
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<crate::Error>,
        extensions: HashMap<String, Value>,
        status_code: Option<StatusCode>,
        headers: MultiMap<IntoHeaderName, IntoHeaderValue>,
        context: Context,
    ) -> Result<RouterResponse, BoxError> {
        let extensions: Object = extensions
            .into_iter()
            .map(|(name, value)| (ByteString::from(name), value))
            .collect();
        // Build a response
        let b = Response::builder()
            .path(path)
            .errors(errors)
            .extensions(extensions);
        let res = match data {
            Some(data) => b.data(data).build(),
            None => b.build(),
        };

        // Build an http Response
        let mut builder = http::Response::builder().status(status_code.unwrap_or(StatusCode::OK));
        for (key, values) in headers {
            let header_name: HeaderName = key.try_into()?;
            for value in values {
                let header_value: HeaderValue = value.try_into()?;
                builder = builder.header(header_name.clone(), header_value);
            }
        }

        let http_response = builder.body(ResponseBody::GraphQL(res))?;

        // Create a compatible Response
        let compat_response = http_compat::Response {
            inner: http_response,
        };

        Ok(Self {
            response: compat_response,
            context,
        })
    }

    /// This is the constructor (or builder) to use when constructing a "fake" RouterResponse.
    ///
    /// This does not enforce the provision of the data that is required for a fully functional
    /// RouterResponse. It's usually enough for testing, when a fully constructed RouterResponse is
    /// difficult to construct and not required for the purposes of the test.
    ///
    /// In addition, fake responses are expected to be valid, and will panic if given invalid values.
    #[allow(clippy::too_many_arguments)]
    pub fn fake_new(
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<crate::Error>,
        extensions: HashMap<String, Value>,
        status_code: Option<StatusCode>,
        headers: MultiMap<IntoHeaderName, IntoHeaderValue>,
        context: Option<Context>,
    ) -> Result<RouterResponse, BoxError> {
        RouterResponse::new(
            data,
            path,
            errors,
            extensions,
            status_code,
            headers,
            context.unwrap_or_default(),
        )
    }

    /// This is the constructor (or builder) to use when constructing a RouterResponse that represents a global error.
    /// It has no path and no response data.
    /// This is useful for things such as authentication errors.
    #[allow(clippy::too_many_arguments)]
    pub fn error_new(
        errors: Vec<crate::Error>,
        status_code: Option<StatusCode>,
        headers: MultiMap<IntoHeaderName, IntoHeaderValue>,
        context: Context,
    ) -> Result<RouterResponse, BoxError> {
        RouterResponse::new(
            Default::default(),
            None,
            errors,
            Default::default(),
            status_code,
            headers,
            context,
        )
    }

    pub fn new_from_response(
        response: http_compat::Response<ResponseBody>,
        context: Context,
    ) -> Self {
        Self { response, context }
    }
}

assert_impl_all!(QueryPlannerRequest: Send);
/// [`Context`] for the request.
pub struct QueryPlannerRequest {
    /// Original request to the Router.
    pub originating_request: http_compat::Request<Request>,

    pub context: Context,
}

#[buildstructor::builder]
impl QueryPlannerRequest {
    /// This is the constructor (or builder) to use when constructing a real QueryPlannerRequest.
    ///
    /// Required parameters are required in non-testing code to create a QueryPlannerRequest.
    pub fn new(
        originating_request: http_compat::Request<Request>,
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

#[buildstructor::builder]
impl QueryPlannerResponse {
    /// This is the constructor (or builder) to use when constructing a real QueryPlannerResponse.
    ///
    /// Required parameters are required in non-testing code to create a QueryPlannerResponse.
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

    pub subgraph_request: http_compat::Request<Request>,

    pub operation_kind: OperationKind,

    pub context: Context,
}

#[buildstructor::builder]
impl SubgraphRequest {
    /// This is the constructor (or builder) to use when constructing a real SubgraphRequest.
    ///
    /// Required parameters are required in non-testing code to create a SubgraphRequest.
    pub fn new(
        originating_request: Arc<http_compat::Request<Request>>,
        subgraph_request: http_compat::Request<Request>,
        operation_kind: OperationKind,
        context: Context,
    ) -> SubgraphRequest {
        Self {
            originating_request,
            subgraph_request,
            operation_kind,
            context,
        }
    }

    /// This is the constructor (or builder) to use when constructing a "fake" SubgraphRequest.
    ///
    /// This does not enforce the provision of the data that is required for a fully functional
    /// SubgraphRequest. It's usually enough for testing, when a fully consructed SubgraphRequest is
    /// difficult to construct and not required for the pusposes of the test.
    pub fn fake_new(
        originating_request: Option<Arc<http_compat::Request<Request>>>,
        subgraph_request: Option<http_compat::Request<Request>>,
        operation_kind: Option<OperationKind>,
        context: Option<Context>,
    ) -> SubgraphRequest {
        SubgraphRequest::new(
            originating_request.unwrap_or_else(|| Arc::new(http_compat::Request::mock())),
            subgraph_request.unwrap_or_else(http_compat::Request::mock),
            operation_kind.unwrap_or(OperationKind::Query),
            context.unwrap_or_default(),
        )
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

#[buildstructor::builder]
impl SubgraphResponse {
    /// This is the constructor to use when constructing a real SubgraphResponse..
    ///
    /// In this case, you already hve a valid response and just wish to associate it with a context
    /// and create a SubgraphResponse.
    pub fn new_from_response(
        response: http_compat::Response<Response>,
        context: Context,
    ) -> SubgraphResponse {
        Self { response, context }
    }

    /// This is the constructor (or builder) to use when constructing a real SubgraphResponse.
    ///
    /// The parameters are not optional, because in a live situation all of these properties must be
    /// set and be correct to create a SubgraphResponse.
    pub fn new(
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

    /// This is the constructor (or builder) to use when constructing a "fake" SubgraphResponse.
    ///
    /// This does not enforce the provision of the data that is required for a fully functional
    /// SubgraphResponse. It's usually enough for testing, when a fully consructed SubgraphResponse is
    /// difficult to construct and not required for the pusposes of the test.
    pub fn fake_new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<crate::Error>,
        extensions: Option<Object>,
        status_code: Option<StatusCode>,
        context: Option<Context>,
    ) -> SubgraphResponse {
        SubgraphResponse::new(
            label,
            data,
            path,
            errors,
            extensions.unwrap_or_default(),
            status_code,
            context.unwrap_or_default(),
        )
    }
}

assert_impl_all!(ExecutionRequest: Send);
/// [`Context`] and [`QueryPlan`] for the request.
pub struct ExecutionRequest {
    /// Original request to the Router.
    pub originating_request: http_compat::Request<Request>,

    pub query_plan: Arc<QueryPlan>,

    pub context: Context,
}

#[buildstructor::builder]
impl ExecutionRequest {
    /// This is the constructor (or builder) to use when constructing a real ExecutionRequest.
    ///
    /// The parameters are not optional, because in a live situation all of these properties must be
    /// set and be correct to create a ExecutionRequest.
    pub fn new(
        originating_request: http_compat::Request<Request>,
        query_plan: Arc<QueryPlan>,
        context: Context,
    ) -> ExecutionRequest {
        Self {
            originating_request,
            query_plan,
            context,
        }
    }

    /// This is the constructor (or builder) to use when constructing a "fake" ExecutionRequest.
    ///
    /// This does not enforce the provision of the data that is required for a fully functional
    /// ExecutionRequest. It's usually enough for testing, when a fully consructed ExecutionRequest is
    /// difficult to construct and not required for the pusposes of the test.
    pub fn fake_new(
        originating_request: Option<http_compat::Request<Request>>,
        query_plan: Option<Arc<QueryPlan>>,
        context: Option<Context>,
    ) -> ExecutionRequest {
        ExecutionRequest::new(
            originating_request.unwrap_or_else(http_compat::Request::mock),
            query_plan.unwrap_or_default(),
            context.unwrap_or_default(),
        )
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

#[buildstructor::builder]
impl ExecutionResponse {
    /// This is the constructor to use when constructing a real ExecutionResponse.
    ///
    /// In this case, you already hve a valid request and just wish to associate it with a context
    /// and create a ExecutionResponse.
    pub fn new_from_response(
        response: http_compat::Response<Response>,
        context: Context,
    ) -> ExecutionResponse {
        Self { response, context }
    }

    /// This is the constructor (or builder) to use when constructing a real RouterRequest.
    ///
    /// The parameters are not optional, because in a live situation all of these properties must be
    /// set and be correct to create a RouterRequest.
    pub fn new(
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

    /// This is the constructor (or builder) to use when constructing a "fake" ExecutionResponse.
    ///
    /// This does not enforce the provision of the data that is required for a fully functional
    /// ExecutionResponse. It's usually enough for testing, when a fully consructed
    /// ExecutionResponse is difficult to construct and not required for the pusposes of the test.
    pub fn fake_new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<crate::Error>,
        extensions: Option<Object>,
        status_code: Option<StatusCode>,
        context: Option<Context>,
    ) -> ExecutionResponse {
        ExecutionResponse::new(
            label,
            data,
            path,
            errors,
            extensions.unwrap_or_default(),
            status_code,
            context.unwrap_or_default(),
        )
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
    fn instrument<F, Request>(
        self,
        span_fn: F,
    ) -> ServiceBuilder<Stack<InstrumentLayer<F, Request>, L>>
    where
        F: Fn(&Request) -> Span,
    {
        self.layer(InstrumentLayer::new(span_fn))
    }
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

#[cfg(test)]
mod test {
    use crate::prelude::graphql;
    use crate::{Context, ResponseBody, RouterRequest, RouterResponse};
    use http::{HeaderValue, Method, Uri};
    use serde_json::json;

    #[test]
    fn router_request_builder() {
        let request = RouterRequest::builder()
            .header("a", "b")
            .header("a", "c")
            .uri(Uri::from_static("http://example.com"))
            .method(Method::POST)
            .query("query { topProducts }")
            .operation_name("Default")
            .context(Context::new())
            // We need to follow up on this. How can users creat this easily?
            .extension("foo", json!({}))
            // We need to follow up on this. How can users creat this easily?
            .variable("bar", json!({}))
            .build()
            .unwrap();
        assert_eq!(
            request
                .originating_request
                .headers()
                .get_all("a")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![HeaderValue::from_static("b"), HeaderValue::from_static("c")]
        );
        assert_eq!(
            request.originating_request.uri(),
            &Uri::from_static("http://example.com")
        );
        assert_eq!(
            request.originating_request.body().extensions.get("foo"),
            Some(&json!({}).into())
        );
        assert_eq!(
            request.originating_request.body().variables.get("bar"),
            Some(&json!({}).into())
        );
        assert_eq!(request.originating_request.method(), Method::POST);

        let extensions = serde_json_bytes::Value::from(json!({"foo":{}}))
            .as_object()
            .unwrap()
            .clone();

        let variables = serde_json_bytes::Value::from(json!({"bar":{}}))
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(
            request.originating_request.body(),
            &graphql::Request::builder()
                .variables(variables)
                .extensions(extensions)
                .operation_name(Some("Default".to_string()))
                .query(Some("query { topProducts }".to_string()))
                .build()
        );
    }

    #[test]
    fn router_response_builder() {
        let response = RouterResponse::builder()
            .header("a", "b")
            .header("a", "c")
            .context(Context::new())
            .extension("foo", json!({}))
            .data(json!({}))
            .build()
            .unwrap();

        assert_eq!(
            response
                .response
                .headers()
                .get_all("a")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![HeaderValue::from_static("b"), HeaderValue::from_static("c")]
        );
        let extensions = serde_json_bytes::Value::from(json!({"foo":{}}))
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(
            response.response.body(),
            &ResponseBody::GraphQL(
                graphql::Response::builder()
                    .extensions(extensions)
                    .data(json!({}))
                    .build()
            )
        );
    }
}
