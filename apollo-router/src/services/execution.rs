#![allow(missing_docs)] // FIXME

use std::sync::Arc;

use futures::future::ready;
use futures::stream::once;
use futures::stream::BoxStream;
use futures::stream::StreamExt;
use http::StatusCode;
use multimap::MultiMap;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JsonMap;
use serde_json_bytes::Value;
use static_assertions::assert_impl_all;
use tower::BoxError;

use crate::error::Error;
use crate::graphql;
use crate::http_ext;
use crate::http_ext::IntoHeaderName;
use crate::http_ext::IntoHeaderValue;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::Context;

pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
pub type ServiceResult = Result<Response, BoxError>;

// Reachable from Request
pub use crate::query_planner::QueryPlan;

assert_impl_all!(Request: Send);
/// [`Context`] and [`QueryPlan`] for the request.
pub struct Request {
    /// Original request to the Router.
    pub originating_request: http_ext::Request<graphql::Request>,

    pub query_plan: Arc<QueryPlan>,

    pub context: Context,
}

#[buildstructor::buildstructor]
impl Request {
    /// This is the constructor (or builder) to use when constructing a real ExecutionRequest.
    ///
    /// The parameters are not optional, because in a live situation all of these properties must be
    /// set and be correct to create a ExecutionRequest.
    #[builder(visibility = "pub")]
    fn new(
        originating_request: http_ext::Request<graphql::Request>,
        query_plan: Arc<QueryPlan>,
        context: Context,
    ) -> Request {
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
    #[builder(visibility = "pub")]
    fn fake_new(
        originating_request: Option<http_ext::Request<graphql::Request>>,
        query_plan: Option<QueryPlan>,
        context: Option<Context>,
    ) -> Request {
        Request::new(
            originating_request.unwrap_or_else(|| {
                http_ext::Request::fake_builder()
                    .headers(Default::default())
                    .body(Default::default())
                    .build()
                    .expect("fake builds should always work; qed")
            }),
            Arc::new(query_plan.unwrap_or_else(|| QueryPlan::fake_builder().build())),
            context.unwrap_or_default(),
        )
    }
}

assert_impl_all!(Response: Send);
/// [`Context`] and [`http_ext::Response<Response>`] for the response.
///
/// This consists of the execution response and the context.
pub struct Response {
    pub response: http_ext::Response<BoxStream<'static, graphql::Response>>,

    pub context: Context,
}

#[buildstructor::buildstructor]
impl Response {
    /// This is the constructor (or builder) to use when constructing a real SupergraphRequest.
    ///
    /// The parameters are not optional, because in a live situation all of these properties must be
    /// set and be correct to create a SupergraphRequest.
    #[builder(visibility = "pub")]
    fn new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
        extensions: Object,
        status_code: Option<StatusCode>,
        context: Context,
    ) -> Self {
        // Build a response
        let res = graphql::Response::builder()
            .and_label(label)
            .data(data.unwrap_or_default())
            .and_path(path)
            .errors(errors)
            .extensions(extensions)
            .build();

        // Build an http Response
        let http_response = http::Response::builder()
            .status(status_code.unwrap_or(StatusCode::OK))
            .body(once(ready(res)).boxed())
            .expect("Response is serializable; qed");

        // Create a compatible Response
        let compat_response = http_ext::Response {
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
    ) -> Self {
        Response::new(
            label,
            data,
            path,
            errors,
            extensions,
            status_code,
            context.unwrap_or_default(),
        )
    }

    /// This is the constructor (or builder) to use when constructing a ExecutionResponse that represents a global error.
    /// It has no path and no response data.
    /// This is useful for things such as authentication errors.
    #[allow(unused_variables)]
    #[builder(visibility = "pub")]
    fn error_new(
        errors: Vec<Error>,
        status_code: Option<StatusCode>,
        headers: MultiMap<IntoHeaderName, IntoHeaderValue>,
        context: Context,
    ) -> Result<Self, BoxError> {
        Ok(Response::new(
            Default::default(),
            Default::default(),
            Default::default(),
            errors,
            Default::default(),
            status_code,
            context,
        ))
    }
}

impl Response {
    /// This is the constructor to use when constructing a real ExecutionResponse.
    ///
    /// In this case, you already have a valid request and just wish to associate it with a context
    /// and create a ExecutionResponse.
    pub fn new_from_response(
        response: http_ext::Response<BoxStream<'static, graphql::Response>>,
        context: Context,
    ) -> Self {
        Self { response, context }
    }

    pub fn map<F>(self, f: F) -> Response
    where
        F: FnOnce(BoxStream<'static, graphql::Response>) -> BoxStream<'static, graphql::Response>,
    {
        Response {
            context: self.context,
            response: self.response.map(f),
        }
    }

    pub fn map_stream(
        self,
        f: impl FnMut(graphql::Response) -> graphql::Response + Send + 'static,
    ) -> Self {
        self.map(move |stream| stream.map(f).boxed())
    }

    pub async fn next_response(&mut self) -> Option<graphql::Response> {
        self.response.body_mut().next().await
    }
}
