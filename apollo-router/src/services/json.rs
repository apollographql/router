use std::pin::Pin;

use futures::future::ready;
use futures::stream::once;
use futures::stream::StreamExt;
use futures::Stream;
use http::header::HeaderName;
use http::method::Method;
use http::HeaderValue;
use http::StatusCode;
use http::Uri;
use mime::APPLICATION_JSON;
use multimap::MultiMap;
use serde_json_bytes::json;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JsonMap;
use serde_json_bytes::Value;
use static_assertions::assert_impl_all;
use tower::BoxError;

use crate::http_ext::header_map;
use crate::http_ext::TryIntoHeaderName;
use crate::http_ext::TryIntoHeaderValue;
use crate::Context;

pub(crate) mod service;
#[cfg(test)]
mod tests;

pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
pub type ServiceResult = Result<Response, BoxError>;

assert_impl_all!(Request: Send);
/// Represents the router processing step of the processing pipeline.
///
/// This consists of the parsed graphql Request, HTTP headers and contextual data for extensions.
#[non_exhaustive]
pub struct Request {
    /// Original request to the Router.
    pub request: http::Request<Value>,

    /// Context for extension
    pub context: Context,
}

impl From<http::Request<Value>> for Request {
    fn from(request: http::Request<Value>) -> Self {
        Self {
            request,
            context: Context::new(),
        }
    }
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("context", &self.context)
            .finish()
    }
}

#[buildstructor::buildstructor]
impl Request {
    /// This is the constructor (or builder) to use when constructing a real Request.
    ///
    /// Required parameters are required in non-testing code to create a Request.
    #[allow(clippy::too_many_arguments)]
    #[builder(visibility = "pub")]
    fn new(
        body: Value,
        context: Context,
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
        uri: Uri,
        method: Method,
    ) -> Result<Request, BoxError> {
        let mut request = http::Request::builder()
            .uri(uri)
            .method(method)
            .body(body)?;
        *request.headers_mut() = header_map(headers)?;
        Ok(Self { request, context })
    }

    /// This is the constructor (or builder) to use when constructing a "fake" Request.
    ///
    /// This does not enforce the provision of the data that is required for a fully functional
    /// Request. It's usually enough for testing, when a fully constructed Request is
    /// difficult to construct and not required for the purposes of the test.
    ///
    /// In addition, fake requests are expected to be valid, and will panic if given invalid values.
    #[builder(visibility = "pub")]
    fn fake_new(
        body: Option<Value>,
        context: Option<Context>,
        mut headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
        method: Option<Method>,
    ) -> Result<Request, BoxError> {
        // Avoid testing requests getting blocked by the CSRF-prevention plugin
        headers
            .entry(http::header::CONTENT_TYPE.into())
            .or_insert(HeaderValue::from_static(APPLICATION_JSON.essence_str()).into());
        let context = context.unwrap_or_default();

        Request::new(
            body.unwrap_or(Value::Null),
            context,
            headers,
            Uri::from_static("http://default"),
            method.unwrap_or(Method::POST),
        )
    }

    /// Create a request with an example query, for tests
    #[builder(visibility = "pub")]
    fn canned_new(
        body: Option<Value>,
        context: Option<Context>,
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
    ) -> Result<Request, BoxError> {
        let default_body = json!({
            "query": "
                query TopProducts($first: Int) {
                    topProducts(first: $first) {
                        upc
                        name
                        reviews {
                            id
                            product { name }
                            author { id name }
                        }
                    }
                }",
            "variables": {
                "first": 2
            }
        });

        Self::fake_new(Some(default_body), context, headers, None)
    }
}

assert_impl_all!(Response: Send);
#[non_exhaustive]
pub struct Response {
    pub response: http::Response<JsonStream>,
    pub context: Context,
}

/// An asynchronous [`Stream`] of JSON objects.
///
/// In some cases such as with `@defer`, a single HTTP response from the Router
/// may contain multiple GraphQL responses that will be sent at different times
/// (as more data becomes available).
///
/// We represent this in Rust as a stream,
/// even if that stream happens to only contain one item.
pub type JsonStream = Pin<Box<dyn Stream<Item = Value> + Send>>;

#[buildstructor::buildstructor]
impl Response {
    /// This is the constructor (or builder) to use when constructing a real Response..
    ///
    /// Required parameters are required in non-testing code to create a Response..
    #[allow(clippy::too_many_arguments)]
    #[builder(visibility = "pub")]
    fn new(
        body: Value,
        status_code: Option<StatusCode>,
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
        context: Context,
    ) -> Result<Self, BoxError> {
        // Build an http Response
        let mut builder = http::Response::builder().status(status_code.unwrap_or(StatusCode::OK));
        for (key, values) in headers {
            let header_name: HeaderName = key.try_into()?;
            for value in values {
                let header_value: HeaderValue = value.try_into()?;
                builder = builder.header(header_name.clone(), header_value);
            }
        }

        let response = builder.body(once(ready(body)).boxed())?;

        Ok(Self { response, context })
    }

    /// This is the constructor (or builder) to use when constructing a "fake" Response.
    ///
    /// This does not enforce the provision of the data that is required for a fully functional
    /// Response. It's usually enough for testing, when a fully constructed Response is
    /// difficult to construct and not required for the purposes of the test.
    ///
    /// In addition, fake responses are expected to be valid, and will panic if given invalid values.
    #[allow(clippy::too_many_arguments)]
    #[builder(visibility = "pub")]
    fn fake_new(
        body: Option<Value>,
        status_code: Option<StatusCode>,
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
        context: Option<Context>,
    ) -> Result<Self, BoxError> {
        Response::new(
            body.unwrap_or(Value::Null),
            status_code,
            headers,
            context.unwrap_or_default(),
        )
    }

    /// This is the constructor (or builder) to use when constructing a "fake" Response stream.
    ///
    /// This does not enforce the provision of the data that is required for a fully functional
    /// Response. It's usually enough for testing, when a fully constructed Response is
    /// difficult to construct and not required for the purposes of the test.
    ///
    /// In addition, fake responses are expected to be valid, and will panic if given invalid values.
    #[builder(visibility = "pub")]
    fn fake_stream_new(
        responses: Vec<Value>,
        status_code: Option<StatusCode>,
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
        context: Context,
    ) -> Result<Self, BoxError> {
        let mut builder = http::Response::builder().status(status_code.unwrap_or(StatusCode::OK));
        for (key, values) in headers {
            let header_name: HeaderName = key.try_into()?;
            for value in values {
                let header_value: HeaderValue = value.try_into()?;
                builder = builder.header(header_name.clone(), header_value);
            }
        }

        let stream = futures::stream::iter(responses);
        let response = builder.body(stream.boxed())?;
        Ok(Self { response, context })
    }

    /// This is the constructor (or builder) to use when constructing a Response that represents a global error.
    /// It has no path and no response data.
    /// This is useful for things such as authentication errors.
    #[builder(visibility = "pub")]
    fn error_new(
        status_code: Option<StatusCode>,
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
        context: Context,
    ) -> Result<Self, BoxError> {
        Response::new(
            Value::Null,
            status_code,
            headers,
            context,
        )
    }

    /// This is the constructor (or builder) to use when constructing a real Response..
    ///
    /// Required parameters are required in non-testing code to create a Response..
    #[allow(clippy::too_many_arguments)]
    #[builder(visibility = "pub(crate)")]
    fn infallible_new(
        body: Value,
        // Skip the `Object` type alias in order to use buildstructor’s map special-casing
        extensions: JsonMap<ByteString, Value>,
        status_code: Option<StatusCode>,
        headers: MultiMap<HeaderName, HeaderValue>,
        context: Context,
    ) -> Self {
        // Build an http Response
        let mut builder = http::Response::builder().status(status_code.unwrap_or(StatusCode::OK));
        for (header_name, values) in headers {
            for header_value in values {
                builder = builder.header(header_name.clone(), header_value);
            }
        }

        let response = builder.body(once(ready(body)).boxed()).expect("can't fail");

        Self { response, context }
    }
}

impl Response {
    pub async fn next_response(&mut self) -> Option<Value> {
        self.response.body_mut().next().await
    }

    pub(crate) fn new_from_response(
        response: http::Response<JsonStream>,
        context: Context,
    ) -> Self {
        Self { response, context }
    }

    pub fn map<F>(self, f: F) -> Response
    where
        F: FnOnce(JsonStream) -> JsonStream,
    {
        Response {
            context: self.context,
            response: self.response.map(f),
        }
    }

    /// Returns a new supergraph response where each [`graphql::Response`] is mapped through `f`.
    ///
    /// In supergraph and execution services, the service response contains
    /// not just one GraphQL response but a stream of them,
    /// in order to support features such as `@defer`.
    /// This method uses [`futures::stream::StreamExt::map`] to map over each item in the stream.
    ///
    /// # Example
    ///
    /// ```
    /// use apollo_router::services::supergraph;
    /// use apollo_router::layers::ServiceExt as _;
    /// use tower::ServiceExt as _;
    ///
    /// struct ExamplePlugin;
    ///
    /// #[async_trait::async_trait]
    /// impl apollo_router::plugin::Plugin for ExamplePlugin {
    ///     # type Config = ();
    ///     # async fn new(
    ///     #     _init: apollo_router::plugin::PluginInit<Self::Config>,
    ///     # ) -> Result<Self, tower::BoxError> {
    ///     #     Ok(Self)
    ///     # }
    ///     // …
    ///     fn supergraph_service(&self, inner: supergraph::BoxService) -> supergraph::BoxService {
    ///         inner
    ///             .map_response(|supergraph_response| {
    ///                 supergraph_response.map_stream(|graphql_response| {
    ///                     // Something interesting here
    ///                     graphql_response
    ///                 })
    ///             })
    ///             .boxed()
    ///     }
    /// }
    /// ```
    pub fn map_stream<F>(self, f: F) -> Self
    where
        F: 'static + Send + FnMut(Value) -> Value,
    {
        self.map(move |stream| stream.map(f).boxed())
    }
}
