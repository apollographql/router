//! Implementation of the various steps in the router's processing pipeline.

use std::collections::HashMap;
use std::sync::Arc;

use futures::future::ready;
use futures::stream::once;
use futures::stream::BoxStream;
use futures::stream::StreamExt;
use http::header::HeaderName;
use http::method::Method;
use http::HeaderValue;
use http::StatusCode;
use http::Uri;
use http_ext::IntoHeaderName;
use http_ext::IntoHeaderValue;
use multimap::MultiMap;
use serde_json_bytes::ByteString;
use static_assertions::assert_impl_all;
pub use subgraph_service::SubgraphService;
use tower::BoxError;

pub use self::execution_service::*;
pub use self::router_service::*;
use crate::error::Error;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::query_planner::fetch::OperationKind;
use crate::query_planner::QueryPlan;
use crate::query_planner::QueryPlanOptions;
use crate::*;

mod execution_service;
pub mod http_ext;
pub(crate) mod layers;
pub mod new_service;
mod router_service;
pub(crate) mod subgraph_service;

assert_impl_all!(RouterRequest: Send);
/// Represents the router processing step of the processing pipeline.
///
/// This consists of the parsed graphql Request, HTTP headers and contextual data for extensions.
pub struct RouterRequest {
    /// Original request to the Router.
    pub originating_request: http_ext::Request<Request>,

    /// Context for extension
    pub context: Context,
}

impl From<http_ext::Request<Request>> for RouterRequest {
    fn from(originating_request: http_ext::Request<Request>) -> Self {
        Self {
            originating_request,
            context: Context::new(),
        }
    }
}

#[buildstructor::buildstructor]
impl RouterRequest {
    /// This is the constructor (or builder) to use when constructing a real RouterRequest.
    ///
    /// Required parameters are required in non-testing code to create a RouterRequest.
    #[allow(clippy::too_many_arguments)]
    #[builder]
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
            .and_query(query)
            .and_operation_name(operation_name)
            .variables(variables)
            .extensions(extensions)
            .build();

        let originating_request = http_ext::Request::builder()
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
    #[builder]
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
/// [`Context`] and [`http_ext::Response<Response>`] for the response.
///
/// This consists of the response body and the context.
pub struct RouterResponse {
    pub response: http_ext::Response<BoxStream<'static, Response>>,
    pub context: Context,
}

#[buildstructor::buildstructor]
impl RouterResponse {
    /// This is the constructor (or builder) to use when constructing a real RouterResponse..
    ///
    /// Required parameters are required in non-testing code to create a RouterResponse..
    #[allow(clippy::too_many_arguments)]
    #[builder]
    pub fn new(
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
        extensions: HashMap<String, Value>,
        status_code: Option<StatusCode>,
        headers: MultiMap<IntoHeaderName, IntoHeaderValue>,
        context: Context,
    ) -> Result<Self, BoxError> {
        let extensions: Object = extensions
            .into_iter()
            .map(|(name, value)| (ByteString::from(name), value))
            .collect();
        // Build a response
        let b = Response::builder()
            .and_path(path)
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

        let http_response = builder.body(once(ready(res)).boxed())?;

        // Create a compatible Response
        let compat_response = http_ext::Response {
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
    #[builder]
    pub fn fake_new(
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
        extensions: HashMap<String, Value>,
        status_code: Option<StatusCode>,
        headers: MultiMap<IntoHeaderName, IntoHeaderValue>,
        context: Option<Context>,
    ) -> Result<Self, BoxError> {
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
    #[builder]
    pub fn error_new(
        errors: Vec<Error>,
        status_code: Option<StatusCode>,
        headers: MultiMap<IntoHeaderName, IntoHeaderValue>,
        context: Context,
    ) -> Result<Self, BoxError> {
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

    pub fn new_from_graphql_response(response: Response, context: Context) -> Self {
        Self {
            response: http::Response::new(once(ready(response)).boxed()).into(),
            context,
        }
    }
}

impl RouterResponse {
    pub async fn next_response(&mut self) -> Option<Response> {
        self.response.body_mut().next().await
    }

    pub fn new_from_response(
        response: http_ext::Response<BoxStream<'static, Response>>,
        context: Context,
    ) -> Self {
        Self { response, context }
    }

    pub fn map<F>(self, f: F) -> RouterResponse
    where
        F: FnMut(BoxStream<'static, Response>) -> BoxStream<'static, Response>,
    {
        RouterResponse {
            context: self.context,
            response: self.response.map(f),
        }
    }
}

assert_impl_all!(QueryPlannerRequest: Send);
/// [`Context`] and [`QueryPlanOptions`] for the request.
#[derive(Clone, Debug)]
pub struct QueryPlannerRequest {
    pub query: String,
    pub operation_name: Option<String>,
    /// Query plan options
    pub query_plan_options: QueryPlanOptions,

    pub context: Context,
}

#[buildstructor::buildstructor]
impl QueryPlannerRequest {
    /// This is the constructor (or builder) to use when constructing a real QueryPlannerRequest.
    ///
    /// Required parameters are required in non-testing code to create a QueryPlannerRequest.
    #[builder]
    pub fn new(
        query: String,
        operation_name: Option<String>,
        query_plan_options: QueryPlanOptions,
        context: Context,
    ) -> QueryPlannerRequest {
        Self {
            query,
            operation_name,
            query_plan_options,
            context,
        }
    }
}

assert_impl_all!(QueryPlannerResponse: Send);
/// [`Context`] and [`QueryPlan`] for the response..
pub struct QueryPlannerResponse {
    pub content: QueryPlannerContent,
    pub context: Context,
}

#[derive(Debug, Clone)]
pub enum QueryPlannerContent {
    Plan {
        query: Arc<Query>,
        plan: Arc<QueryPlan>,
    },
    Introspection {
        response: Box<Response>,
    },
    IntrospectionDisabled,
}

#[buildstructor::buildstructor]
impl QueryPlannerResponse {
    /// This is the constructor (or builder) to use when constructing a real QueryPlannerResponse.
    ///
    /// Required parameters are required in non-testing code to create a QueryPlannerResponse.
    #[builder]
    pub fn new(content: QueryPlannerContent, context: Context) -> QueryPlannerResponse {
        Self { content, context }
    }

    /// This is the constructor (or builder) to use when constructing a QueryPlannerResponse that represents a global error.
    /// It has no path and no response data.
    /// This is useful for things such as authentication errors.
    #[allow(unused_variables)]
    #[builder]
    pub fn error_new(
        errors: Vec<Error>,
        status_code: Option<StatusCode>,
        headers: MultiMap<IntoHeaderName, IntoHeaderValue>,
        context: Context,
    ) -> Result<QueryPlannerResponse, BoxError> {
        tracing::warn!("no way to propagate error response from QueryPlanner");
        Ok(QueryPlannerResponse::new(
            QueryPlannerContent::Plan {
                plan: Arc::new(QueryPlan::fake_builder().build()),
                query: Arc::new(Query::default()),
            },
            context,
        ))
    }
}

assert_impl_all!(SubgraphRequest: Send);
/// [`Context`], [`OperationKind`] and [`http_ext::Request<Request>`] for the request.
pub struct SubgraphRequest {
    /// Original request to the Router.
    pub originating_request: Arc<http_ext::Request<Request>>,

    pub subgraph_request: http_ext::Request<Request>,

    pub operation_kind: OperationKind,

    pub context: Context,
}

#[buildstructor::buildstructor]
impl SubgraphRequest {
    /// This is the constructor (or builder) to use when constructing a real SubgraphRequest.
    ///
    /// Required parameters are required in non-testing code to create a SubgraphRequest.
    #[builder]
    pub fn new(
        originating_request: Arc<http_ext::Request<Request>>,
        subgraph_request: http_ext::Request<Request>,
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
    #[builder]
    pub fn fake_new(
        originating_request: Option<Arc<http_ext::Request<Request>>>,
        subgraph_request: Option<http_ext::Request<Request>>,
        operation_kind: Option<OperationKind>,
        context: Option<Context>,
    ) -> SubgraphRequest {
        SubgraphRequest::new(
            originating_request.unwrap_or_else(|| {
                Arc::new(
                    http_ext::Request::fake_builder()
                        .headers(Default::default())
                        .body(Default::default())
                        .build()
                        .expect("fake builds should always work; qed"),
                )
            }),
            subgraph_request.unwrap_or_else(|| {
                http_ext::Request::fake_builder()
                    .headers(Default::default())
                    .body(Default::default())
                    .build()
                    .expect("fake builds should always work; qed")
            }),
            operation_kind.unwrap_or(OperationKind::Query),
            context.unwrap_or_default(),
        )
    }
}

assert_impl_all!(SubgraphResponse: Send);
/// [`Context`] and [`http_ext::Response<Response>`] for the response.
///
/// This consists of the subgraph response and the context.
#[derive(Clone, Debug)]
pub struct SubgraphResponse {
    pub response: http_ext::Response<Response>,

    pub context: Context,
}

#[buildstructor::buildstructor]
impl SubgraphResponse {
    /// This is the constructor to use when constructing a real SubgraphResponse..
    ///
    /// In this case, you already have a valid response and just wish to associate it with a context
    /// and create a SubgraphResponse.
    pub fn new_from_response(
        response: http_ext::Response<Response>,
        context: Context,
    ) -> SubgraphResponse {
        Self { response, context }
    }

    /// This is the constructor (or builder) to use when constructing a real SubgraphResponse.
    ///
    /// The parameters are not optional, because in a live situation all of these properties must be
    /// set and be correct to create a SubgraphResponse.
    #[builder]
    pub fn new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
        extensions: Object,
        status_code: Option<StatusCode>,
        context: Context,
    ) -> SubgraphResponse {
        // Build a response
        let res = Response::builder()
            .and_label(label)
            .data(data.unwrap_or_default())
            .and_path(path)
            .errors(errors)
            .extensions(extensions)
            .build();

        // Build an http Response
        let http_response = http::Response::builder()
            .status(status_code.unwrap_or(StatusCode::OK))
            .body(res)
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

    /// This is the constructor (or builder) to use when constructing a "fake" SubgraphResponse.
    ///
    /// This does not enforce the provision of the data that is required for a fully functional
    /// SubgraphResponse. It's usually enough for testing, when a fully consructed SubgraphResponse is
    /// difficult to construct and not required for the pusposes of the test.
    #[builder]
    pub fn fake_new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
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

    /// This is the constructor (or builder) to use when constructing a SubgraphResponse that represents a global error.
    /// It has no path and no response data.
    /// This is useful for things such as authentication errors.
    #[builder]
    pub fn error_new(
        errors: Vec<Error>,
        status_code: Option<StatusCode>,
        context: Context,
    ) -> Result<SubgraphResponse, BoxError> {
        Ok(SubgraphResponse::new(
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

assert_impl_all!(ExecutionRequest: Send);
/// [`Context`] and [`QueryPlan`] for the request.
pub struct ExecutionRequest {
    /// Original request to the Router.
    pub originating_request: http_ext::Request<Request>,

    pub query_plan: Arc<QueryPlan>,

    pub context: Context,
}

#[buildstructor::buildstructor]
impl ExecutionRequest {
    /// This is the constructor (or builder) to use when constructing a real ExecutionRequest.
    ///
    /// The parameters are not optional, because in a live situation all of these properties must be
    /// set and be correct to create a ExecutionRequest.
    #[builder]
    pub fn new(
        originating_request: http_ext::Request<Request>,
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
    #[builder]
    pub fn fake_new(
        originating_request: Option<http_ext::Request<Request>>,
        query_plan: Option<QueryPlan>,
        context: Option<Context>,
    ) -> ExecutionRequest {
        ExecutionRequest::new(
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

assert_impl_all!(ExecutionResponse: Send);
/// [`Context`] and [`http_ext::Response<Response>`] for the response.
///
/// This consists of the execution response and the context.
pub struct ExecutionResponse {
    pub response: http_ext::Response<BoxStream<'static, Response>>,

    pub context: Context,
}

#[buildstructor::buildstructor]
impl ExecutionResponse {
    /// This is the constructor (or builder) to use when constructing a real RouterRequest.
    ///
    /// The parameters are not optional, because in a live situation all of these properties must be
    /// set and be correct to create a RouterRequest.
    #[builder]
    pub fn new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
        extensions: Object,
        status_code: Option<StatusCode>,
        context: Context,
    ) -> Self {
        // Build a response
        let res = Response::builder()
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
    #[builder]
    pub fn fake_new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
        extensions: Option<Object>,
        status_code: Option<StatusCode>,
        context: Option<Context>,
    ) -> Self {
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

    /// This is the constructor (or builder) to use when constructing a ExecutionResponse that represents a global error.
    /// It has no path and no response data.
    /// This is useful for things such as authentication errors.
    #[allow(unused_variables)]
    #[builder]
    pub fn error_new(
        errors: Vec<Error>,
        status_code: Option<StatusCode>,
        headers: MultiMap<IntoHeaderName, IntoHeaderValue>,
        context: Context,
    ) -> Result<Self, BoxError> {
        Ok(ExecutionResponse::new(
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

impl ExecutionResponse {
    /// This is the constructor to use when constructing a real ExecutionResponse.
    ///
    /// In this case, you already have a valid request and just wish to associate it with a context
    /// and create a ExecutionResponse.
    pub fn new_from_response(
        response: http_ext::Response<BoxStream<'static, Response>>,
        context: Context,
    ) -> Self {
        Self { response, context }
    }

    pub fn map<F>(self, f: F) -> ExecutionResponse
    where
        F: FnMut(BoxStream<'static, Response>) -> BoxStream<'static, Response>,
    {
        ExecutionResponse {
            context: self.context,
            response: self.response.map(f),
        }
    }

    pub async fn next_response(&mut self) -> Option<Response> {
        self.response.body_mut().next().await
    }
}

impl AsRef<Request> for http_ext::Request<Request> {
    fn as_ref(&self) -> &Request {
        self.body()
    }
}

impl AsRef<Request> for Arc<http_ext::Request<Request>> {
    fn as_ref(&self) -> &Request {
        self.body()
    }
}

#[cfg(test)]
mod test {
    use http::HeaderValue;
    use http::Method;
    use http::Uri;
    use serde_json::json;

    use crate::graphql;
    use crate::Context;
    use crate::RouterRequest;
    use crate::RouterResponse;

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
                .operation_name("Default")
                .query("query { topProducts }")
                .build()
        );
    }

    #[tokio::test]
    async fn router_response_builder() {
        let mut response = RouterResponse::builder()
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
            response.next_response().await.unwrap(),
            graphql::Response::builder()
                .extensions(extensions)
                .data(json!({}))
                .build()
        );
    }
}
