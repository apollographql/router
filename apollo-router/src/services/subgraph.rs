#![allow(missing_docs)] // FIXME

use std::sync::Arc;

use futures::channel::mpsc;
use http::StatusCode;
use http::Version;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JsonMap;
use serde_json_bytes::Value;
use sha2::Digest;
use sha2::Sha256;
use static_assertions::assert_impl_all;
use tokio::sync::broadcast;
use tower::BoxError;

use crate::error::Error;
use crate::graphql;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::notification::HandleStream;
use crate::plugins::authentication::APOLLO_AUTHENTICATION_JWT_CLAIMS;
use crate::query_planner::fetch::OperationKind;
use crate::Context;

pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
pub type ServiceResult = Result<Response, BoxError>;

assert_impl_all!(Request: Send);
#[non_exhaustive]
pub struct Request {
    /// Original request to the Router.
    pub supergraph_request: Arc<http::Request<graphql::Request>>,

    pub subgraph_request: http::Request<graphql::Request>,

    pub operation_kind: OperationKind,

    pub context: Context,

    /// Channel to send the subscription stream to listen on events coming from subgraph in a task
    pub(crate) subscription_stream: Option<mpsc::Sender<HandleStream<String, graphql::Response>>>,
    /// Channel triggered when the client connection has been dropped
    pub(crate) connection_closed_signal: Option<broadcast::Receiver<()>>,
}

#[buildstructor::buildstructor]
impl Request {
    /// This is the constructor (or builder) to use when constructing a real Request.
    ///
    /// Required parameters are required in non-testing code to create a Request.
    #[builder(visibility = "pub")]
    fn new(
        supergraph_request: Arc<http::Request<graphql::Request>>,
        subgraph_request: http::Request<graphql::Request>,
        operation_kind: OperationKind,
        context: Context,
        subscription_stream: Option<mpsc::Sender<HandleStream<String, graphql::Response>>>,
        connection_closed_signal: Option<broadcast::Receiver<()>>,
    ) -> Request {
        Self {
            supergraph_request,
            subgraph_request,
            operation_kind,
            context,
            subscription_stream,
            connection_closed_signal,
        }
    }

    /// This is the constructor (or builder) to use when constructing a "fake" Request.
    ///
    /// This does not enforce the provision of the data that is required for a fully functional
    /// Request. It's usually enough for testing, when a fully consructed Request is
    /// difficult to construct and not required for the pusposes of the test.
    #[builder(visibility = "pub")]
    fn fake_new(
        supergraph_request: Option<Arc<http::Request<graphql::Request>>>,
        subgraph_request: Option<http::Request<graphql::Request>>,
        operation_kind: Option<OperationKind>,
        context: Option<Context>,
        subscription_stream: Option<mpsc::Sender<HandleStream<String, graphql::Response>>>,
        connection_closed_signal: Option<broadcast::Receiver<()>>,
    ) -> Request {
        Request::new(
            supergraph_request.unwrap_or_default(),
            subgraph_request.unwrap_or_default(),
            operation_kind.unwrap_or(OperationKind::Query),
            context.unwrap_or_default(),
            subscription_stream,
            connection_closed_signal,
        )
    }
}

impl Clone for Request {
    fn clone(&self) -> Self {
        // http::Request is not clonable so we have to rebuild a new one
        // we don't use the extensions field for now
        let mut builder = http::Request::builder()
            .method(self.subgraph_request.method())
            .version(self.subgraph_request.version())
            .uri(self.subgraph_request.uri());

        {
            let headers = builder.headers_mut().unwrap();
            headers.extend(
                self.subgraph_request
                    .headers()
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone())),
            );
        }
        let subgraph_request = builder.body(self.subgraph_request.body().clone()).unwrap();

        Self {
            supergraph_request: self.supergraph_request.clone(),
            subgraph_request,
            operation_kind: self.operation_kind,
            context: self.context.clone(),
            subscription_stream: self.subscription_stream.clone(),
            connection_closed_signal: self
                .connection_closed_signal
                .as_ref()
                .map(|s| s.resubscribe()),
        }
    }
}

assert_impl_all!(Response: Send);
#[derive(Debug)]
#[non_exhaustive]
pub struct Response {
    pub response: http::Response<graphql::Response>,

    pub context: Context,
}

#[buildstructor::buildstructor]
impl Response {
    /// This is the constructor to use when constructing a real Response..
    ///
    /// In this case, you already have a valid response and just wish to associate it with a context
    /// and create a Response.
    pub(crate) fn new_from_response(
        response: http::Response<graphql::Response>,
        context: Context,
    ) -> Response {
        Self { response, context }
    }

    /// This is the constructor (or builder) to use when constructing a real Response.
    ///
    /// The parameters are not optional, because in a live situation all of these properties must be
    /// set and be correct to create a Response.
    #[builder(visibility = "pub")]
    fn new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
        extensions: Object,
        status_code: Option<StatusCode>,
        context: Context,
        headers: Option<http::HeaderMap<http::HeaderValue>>,
    ) -> Response {
        // Build a response
        let res = graphql::Response::builder()
            .and_label(label)
            .data(data.unwrap_or_default())
            .and_path(path)
            .errors(errors)
            .extensions(extensions)
            .build();

        // Build an http Response
        let mut response = http::Response::builder()
            .status(status_code.unwrap_or(StatusCode::OK))
            .body(res)
            .expect("Response is serializable; qed");

        *response.headers_mut() = headers.unwrap_or_default();

        Self { response, context }
    }

    /// This is the constructor (or builder) to use when constructing a "fake" Response.
    ///
    /// This does not enforce the provision of the data that is required for a fully functional
    /// Response. It's usually enough for testing, when a fully consructed Response is
    /// difficult to construct and not required for the pusposes of the test.
    #[builder(visibility = "pub")]
    fn fake_new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
        // Skip the `Object` type alias in order to use buildstructor’s map special-casing
        extensions: JsonMap<ByteString, Value>,
        status_code: Option<StatusCode>,
        context: Option<Context>,
        headers: Option<http::HeaderMap<http::HeaderValue>>,
    ) -> Response {
        Response::new(
            label,
            data,
            path,
            errors,
            extensions,
            status_code,
            context.unwrap_or_default(),
            headers,
        )
    }

    /// This is the constructor (or builder) to use when constructing a Response that represents a global error.
    /// It has no path and no response data.
    /// This is useful for things such as authentication errors.
    #[builder(visibility = "pub")]
    fn error_new(
        errors: Vec<Error>,
        status_code: Option<StatusCode>,
        context: Context,
    ) -> Result<Response, BoxError> {
        Ok(Response::new(
            Default::default(),
            Default::default(),
            Default::default(),
            errors,
            Default::default(),
            status_code,
            context,
            Default::default(),
        ))
    }
}

impl Request {
    #[allow(dead_code)]
    pub(crate) fn to_sha256(&self) -> String {
        let mut hasher = Sha256::new();
        let http_req = &self.subgraph_request;
        hasher.update(http_req.method().as_str().as_bytes());

        // To not allocate
        let version = match http_req.version() {
            Version::HTTP_09 => "HTTP/0.9",
            Version::HTTP_10 => "HTTP/1.0",
            Version::HTTP_11 => "HTTP/1.1",
            Version::HTTP_2 => "HTTP/2.0",
            Version::HTTP_3 => "HTTP/3.0",
            _ => "unknown",
        };
        hasher.update(version.as_bytes());
        let uri = http_req.uri();
        if let Some(scheme) = uri.scheme() {
            hasher.update(scheme.as_str().as_bytes());
        }
        if let Some(authority) = uri.authority() {
            hasher.update(authority.as_str().as_bytes());
        }
        if let Some(query) = uri.query() {
            hasher.update(query.as_bytes());
        }

        // this assumes headers are in the same order
        for (name, value) in http_req.headers() {
            hasher.update(name.as_str().as_bytes());
            hasher.update(value.to_str().unwrap_or("ERROR").as_bytes());
        }
        if let Some(claim) = self
            .context
            .get_json_value(APOLLO_AUTHENTICATION_JWT_CLAIMS)
        {
            hasher.update(format!("{claim:?}").as_bytes());
        }
        let body = http_req.body();
        if let Some(operation_name) = &body.operation_name {
            hasher.update(operation_name.as_bytes());
        }
        if let Some(query) = &body.query {
            hasher.update(query.as_bytes());
        }
        for (var_name, var_value) in &body.variables {
            hasher.update(var_name.inner());
            // TODO implement to_bytes() for value in serde_json_bytes
            hasher.update(var_value.to_string().as_bytes());
        }
        for (name, val) in &body.extensions {
            hasher.update(name.inner());
            // TODO implement to_bytes() for value in serde_json_bytes
            hasher.update(val.to_string().as_bytes());
        }

        hex::encode(hasher.finalize())
    }
}
