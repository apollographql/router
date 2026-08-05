#![allow(missing_docs)] // FIXME

use std::any::Any;
use std::mem;

use ahash::HashMap;
use apollo_json::NewValue;
use bytes::Bytes;
use displaydoc::Display;
use futures::Stream;
use futures::StreamExt;
use futures::future::Either;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use http::header::CONTENT_TYPE;
use http::header::HeaderName;
use http_body_util::BodyExt;
use multer::Multipart;
use multimap::MultiMap;
use static_assertions::assert_impl_all;
use thiserror::Error;
use tower::BoxError;
use uuid::Uuid;

use self::body::RouterBody;
use self::service::MULTIPART_DEFER_CONTENT_TYPE_HEADER_VALUE;
use self::service::MULTIPART_SUBSCRIPTION_CONTENT_TYPE_HEADER_VALUE;
use super::supergraph;
use crate::Context;
use crate::context::CHUNK_CONTAINS_GRAPHQL_ERROR;
use crate::context::CONTAINS_GRAPHQL_ERROR;
use crate::context::ROUTER_RESPONSE_ERRORS;
use crate::graphql;
use crate::graphql::json_object::ObjectAccumulator;
use crate::graphql::json_object::empty_object;
use crate::http_ext::header_map;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::plugins::telemetry::config_new::router::events::RouterResponseBodyExtensionType;
use crate::services::TryIntoHeaderName;
use crate::services::TryIntoHeaderValue;

pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
pub type ServiceResult = Result<Response, BoxError>;

pub type Body = RouterBody;
pub type Error = hyper::Error;

mod batching;
pub mod body;
pub(crate) mod parse_query;
pub(crate) mod pipeline_handle;
pub(crate) mod service;
#[cfg(test)]
mod tests;
pub(crate) mod tower_compat;

assert_impl_all!(Request: Send);
/// Represents the router processing step of the processing pipeline.
///
/// This consists of the parsed graphql Request, HTTP headers and contextual data for extensions.
#[non_exhaustive]
pub struct Request {
    /// Original request to the Router.
    pub router_request: http::Request<Body>,

    /// Context for extension
    pub context: Context,
}

impl From<(http::Request<Body>, Context)> for Request {
    fn from((router_request, context): (http::Request<Body>, Context)) -> Self {
        Self {
            router_request,
            context,
        }
    }
}

/// Helper type to conveniently construct a body from several types used commonly in tests.
///
/// It's only meant for integration tests, as the "real" router should create bodies explicitly accounting for
/// streaming, size limits, etc.
pub struct IntoBody(Body);

impl From<Body> for IntoBody {
    fn from(value: Body) -> Self {
        Self(value)
    }
}
impl From<String> for IntoBody {
    fn from(value: String) -> Self {
        Self(body::from_bytes(value))
    }
}
impl From<Bytes> for IntoBody {
    fn from(value: Bytes) -> Self {
        Self(body::from_bytes(value))
    }
}
impl From<Vec<u8>> for IntoBody {
    fn from(value: Vec<u8>) -> Self {
        Self(body::from_bytes(value))
    }
}

impl Request {
    /// Starts building a request for the router service.
    ///
    /// `context`, `uri`, `method` and `body` have no sensible default and must be set before
    /// [`RequestBuilder::build`].
    pub fn builder() -> RequestBuilder {
        RequestBuilder::default()
    }

    /// Starts building a request with test-friendly defaults: an empty context, a
    /// `http://example.com/` URI, `GET`, and an empty body.
    pub fn fake_builder() -> FakeRequestBuilder {
        FakeRequestBuilder::default()
    }

    fn new(
        context: Context,
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
        uri: http::Uri,
        method: Method,
        body: Body,
    ) -> Result<Request, BoxError> {
        let mut router_request = http::Request::builder()
            .uri(uri)
            .method(method)
            .body(body)?;
        *router_request.headers_mut() = header_map(headers)?;
        Ok(Self {
            router_request,
            context,
        })
    }

    fn fake_new(
        context: Option<Context>,
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
        uri: Option<http::Uri>,
        method: Option<Method>,
        body: Option<IntoBody>,
    ) -> Result<Request, BoxError> {
        let mut router_request = http::Request::builder()
            .uri(uri.unwrap_or_else(|| http::Uri::from_static("http://example.com/")))
            .method(method.unwrap_or(Method::GET))
            .body(body.map_or_else(body::empty, |constructed| constructed.0))?;
        *router_request.headers_mut() = header_map(headers)?;
        Ok(Self {
            router_request,
            context: context.unwrap_or_default(),
        })
    }
}

/// Builds a [`Request`]. Created by [`Request::builder`].
#[derive(Default)]
pub struct RequestBuilder {
    context: Option<Context>,
    headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
    uri: Option<http::Uri>,
    method: Option<Method>,
    body: Option<Body>,
}

impl RequestBuilder {
    pub fn context(mut self, context: impl Into<Context>) -> Self {
        self.context = Some(context.into());
        self
    }

    pub fn headers(mut self, headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>) -> Self {
        self.headers.extend(headers);
        self
    }

    pub fn header(
        mut self,
        name: impl Into<TryIntoHeaderName>,
        value: impl Into<TryIntoHeaderValue>,
    ) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    pub fn uri(mut self, uri: impl Into<http::Uri>) -> Self {
        self.uri = Some(uri.into());
        self
    }

    pub fn method(mut self, method: impl Into<Method>) -> Self {
        self.method = Some(method.into());
        self
    }

    pub fn body(mut self, body: impl Into<Body>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// # Panics
    ///
    /// Panics unless `context`, `uri`, `method` and `body` have all been set.
    pub fn build(self) -> Result<Request, BoxError> {
        Request::new(
            self.context.expect("context is required"),
            self.headers,
            self.uri.expect("uri is required"),
            self.method.expect("method is required"),
            self.body.expect("body is required"),
        )
    }
}

/// Builds a [`Request`] with test-friendly defaults. Created by [`Request::fake_builder`].
#[derive(Default)]
pub struct FakeRequestBuilder {
    context: Option<Context>,
    headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
    uri: Option<http::Uri>,
    method: Option<Method>,
    body: Option<IntoBody>,
}

impl FakeRequestBuilder {
    pub fn context(mut self, context: impl Into<Context>) -> Self {
        self.context = Some(context.into());
        self
    }

    pub fn and_context(mut self, context: Option<impl Into<Context>>) -> Self {
        self.context = context.map(Into::into);
        self
    }

    pub fn headers(mut self, headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>) -> Self {
        self.headers.extend(headers);
        self
    }

    pub fn header(
        mut self,
        name: impl Into<TryIntoHeaderName>,
        value: impl Into<TryIntoHeaderValue>,
    ) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    pub fn uri(mut self, uri: impl Into<http::Uri>) -> Self {
        self.uri = Some(uri.into());
        self
    }

    pub fn and_uri(mut self, uri: Option<impl Into<http::Uri>>) -> Self {
        self.uri = uri.map(Into::into);
        self
    }

    pub fn method(mut self, method: impl Into<Method>) -> Self {
        self.method = Some(method.into());
        self
    }

    pub fn and_method(mut self, method: Option<impl Into<Method>>) -> Self {
        self.method = method.map(Into::into);
        self
    }

    pub fn body(mut self, body: impl Into<IntoBody>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn and_body(mut self, body: Option<impl Into<IntoBody>>) -> Self {
        self.body = body.map(Into::into);
        self
    }

    pub fn build(self) -> Result<Request, BoxError> {
        Request::fake_new(self.context, self.headers, self.uri, self.method, self.body)
    }
}

#[derive(Error, Display, Debug)]
pub enum ParseError {
    /// couldn't create a valid http GET uri '{0}'
    InvalidUri(http::uri::InvalidUri),
    /// couldn't urlencode the GraphQL request body '{0}'
    UrlEncodeError(serde_urlencoded::ser::Error),
    /// couldn't serialize the GraphQL request body '{0}'
    SerializationError(serde_json::Error),
}

/// This is handy for tests.
impl TryFrom<supergraph::Request> for Request {
    type Error = ParseError;
    fn try_from(request: supergraph::Request) -> Result<Self, Self::Error> {
        let supergraph::Request {
            context,
            supergraph_request,
            ..
        } = request;

        let (mut parts, request) = supergraph_request.into_parts();

        let router_request = if parts.method == Method::GET {
            // get request
            let get_path = serde_urlencoded::to_string([
                ("query", request.query),
                ("operationName", request.operation_name),
                (
                    "extensions",
                    serde_json::to_string(&request.extensions).ok(),
                ),
                ("variables", serde_json::to_string(&request.variables).ok()),
            ])
            .map_err(ParseError::UrlEncodeError)?;

            parts.uri = format!("{}?{}", parts.uri, get_path)
                .parse()
                .map_err(ParseError::InvalidUri)?;

            http::Request::from_parts(parts, body::empty())
        } else {
            http::Request::from_parts(
                parts,
                body::from_bytes(
                    serde_json::to_vec(&request).map_err(ParseError::SerializationError)?,
                ),
            )
        };
        Ok(Self {
            router_request,
            context,
        })
    }
}

assert_impl_all!(Response: Send);
#[non_exhaustive]
#[derive(Debug)]
pub struct Response {
    pub response: http::Response<Body>,
    pub context: Context,
}

impl Response {
    /// Starts building a response for the router service, serializing a single GraphQL
    /// response into the body.
    ///
    /// `context` has no sensible default and must be set before [`ResponseBuilder::build`].
    /// Supplying any error also records it on the context for telemetry.
    pub fn builder() -> ResponseBuilder {
        ResponseBuilder::default()
    }

    /// Starts building a response with test-friendly defaults: an empty context and a
    /// `200 OK` status.
    pub fn fake_builder() -> FakeResponseBuilder {
        FakeResponseBuilder::default()
    }

    /// Starts building a response around an HTTP response whose body is already encoded.
    ///
    /// `response` and `context` have no sensible default and must be set before
    /// [`HttpResponseBuilder::build`].
    pub fn http_response_builder() -> HttpResponseBuilder {
        HttpResponseBuilder::default()
    }

    /// Starts building a response carrying only errors, with no data and no path — the shape
    /// a request-level rejection such as an authentication failure takes.
    ///
    /// `context` has no sensible default and must be set before
    /// [`ErrorResponseBuilder::build`].
    pub fn error_builder() -> ErrorResponseBuilder {
        ErrorResponseBuilder::default()
    }

    /// Starts building a response from already-valid headers, so that building cannot fail.
    ///
    /// `context` has no sensible default and must be set before
    /// [`InfallibleResponseBuilder::build`].
    pub(crate) fn infallible_builder() -> InfallibleResponseBuilder {
        InfallibleResponseBuilder::default()
    }

    fn stash_the_body_in_extensions(&mut self, body_string: String) {
        self.context.extensions().with_lock(|ext| {
            ext.insert(RouterResponseBodyExtensionType(body_string));
        });
    }

    pub async fn next_response(&mut self) -> Option<Result<Bytes, axum::Error>> {
        self.response.body_mut().into_data_stream().next().await
    }

    fn new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<graphql::Error>,
        extensions: Value,
        status_code: Option<StatusCode>,
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
        context: Context,
    ) -> Result<Self, BoxError> {
        if !errors.is_empty() {
            Self::add_errors_to_context(&errors, &context);
        }

        // Build a response
        let b = graphql::Response::builder()
            .and_label(label)
            .and_path(path)
            .errors(errors)
            .extensions(extensions);
        let res = match data {
            Some(data) => b.data(data).build(),
            None => b.build(),
        };

        // Build an HTTP Response
        let mut builder = http::Response::builder().status(status_code.unwrap_or(StatusCode::OK));
        for (key, values) in headers {
            let header_name: HeaderName = key.try_into()?;
            for value in values {
                let header_value: HeaderValue = value.try_into()?;
                builder = builder.header(header_name.clone(), header_value);
            }
        }

        let body_string = serde_json::to_string(&res)?;

        let body = body::from_bytes(body_string.clone());
        let response = builder.body(body)?;
        // Stash the body in the extensions so we can access it later
        let mut response = Self { response, context };
        response.stash_the_body_in_extensions(body_string);

        Ok(response)
    }

    fn http_response_new(
        response: http::Response<Body>,
        context: Context,
        body_to_stash: Option<String>,
        errors_for_context: Option<Vec<graphql::Error>>,
    ) -> Result<Self, BoxError> {
        // There are instances where we have errors that need to be counted for telemetry in this
        // layer, but we don't want to deserialize the body. In these cases we can pass in the
        // list of errors to add to context for counting later in the telemetry plugin.
        if let Some(errors) = errors_for_context
            && !errors.is_empty()
        {
            Self::add_errors_to_context(&errors, &context);
        }
        let mut res = Self { response, context };
        if let Some(body_to_stash) = body_to_stash {
            res.stash_the_body_in_extensions(body_to_stash)
        }
        Ok(res)
    }

    fn error_new(
        errors: Vec<graphql::Error>,
        status_code: Option<StatusCode>,
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
        context: Context,
    ) -> Result<Self, BoxError> {
        Response::new(
            Default::default(),
            Default::default(),
            None,
            errors,
            empty_object(),
            status_code,
            headers,
            context,
        )
    }

    fn infallible_new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<graphql::Error>,
        extensions: Value,
        status_code: Option<StatusCode>,
        headers: MultiMap<HeaderName, HeaderValue>,
        context: Context,
    ) -> Self {
        if !errors.is_empty() {
            Self::add_errors_to_context(&errors, &context);
        }

        // Build a response
        let b = graphql::Response::builder()
            .and_label(label)
            .and_path(path)
            .errors(errors)
            .extensions(extensions);
        let res = match data {
            Some(data) => b.data(data).build(),
            None => b.build(),
        };

        // Build an http Response
        let mut builder = http::Response::builder().status(status_code.unwrap_or(StatusCode::OK));
        for (header_name, values) in headers {
            for header_value in values {
                builder = builder.header(header_name.clone(), header_value);
            }
        }

        let body_string = serde_json::to_string(&res).expect("JSON is always a valid string");

        let body = body::from_bytes(body_string.clone());
        let response = builder.body(body).expect("RouterBody is always valid");

        Self { response, context }
    }

    fn add_errors_to_context(errors: &[graphql::Error], context: &Context) {
        context.insert_json_value(CONTAINS_GRAPHQL_ERROR, Value::from(true));
        context.insert_json_value(CHUNK_CONTAINS_GRAPHQL_ERROR, Value::from(true));
        // This is ONLY guaranteed to capture errors if any were added during router service
        // processing. We will sometimes avoid this path if no router service errors exist, even
        // if errors were passed from the supergraph service, because that path builds the
        // router::Response using parts_new(). This is ok because we only need this context to
        // count errors introduced in the router service; however, it means that we handle error
        // counting differently in this layer than others.
        context
            .insert(
                ROUTER_RESPONSE_ERRORS,
                // We can't serialize the apollo_id, so make a map with id as the key
                errors
                    .iter()
                    .cloned()
                    .map(|err| (err.apollo_id(), err))
                    .collect::<HashMap<Uuid, graphql::Error>>(),
            )
            .expect("Unable to serialize router response errors list for context");
    }

    /// EXPERIMENTAL: THIS FUNCTION IS EXPERIMENTAL AND SUBJECT TO POTENTIAL CHANGE.
    ///
    /// Each item is one GraphQL response parsed from the body, or the reason
    /// that part of the body was not a GraphQL response.
    pub async fn into_graphql_response_stream(
        self,
    ) -> impl Stream<Item = Result<graphql::Response, graphql::MalformedResponseError>> {
        Box::pin(
            if self
                .response
                .headers()
                .get(CONTENT_TYPE)
                .iter()
                .any(|value| {
                    *value == MULTIPART_DEFER_CONTENT_TYPE_HEADER_VALUE
                        || *value == MULTIPART_SUBSCRIPTION_CONTENT_TYPE_HEADER_VALUE
                })
            {
                let multipart = Multipart::new(
                    http_body_util::BodyDataStream::new(self.response.into_body()),
                    "graphql",
                );

                Either::Left(futures::stream::unfold(multipart, |mut m| async {
                    if let Ok(Some(response)) = m.next_field().await
                        && let Ok(bytes) = response.bytes().await
                    {
                        return Some((graphql::Response::from_bytes(bytes), m));
                    }
                    None
                }))
            } else {
                let mut body = http_body_util::BodyDataStream::new(self.response.into_body());
                let res = body.next().await.and_then(|res| res.ok());

                Either::Right(
                    futures::stream::iter(res).map(graphql::Response::from_bytes),
                )
            },
        )
    }

    fn fake_new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<graphql::Error>,
        extensions: Value,
        status_code: Option<StatusCode>,
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
        context: Option<Context>,
    ) -> Result<Self, BoxError> {
        // Build a response
        Self::new(
            label,
            data,
            path,
            errors,
            extensions,
            status_code,
            headers,
            context.unwrap_or_default(),
        )
    }
}

/// Builds a [`Response`]. Created by [`Response::builder`].
#[derive(Default)]
pub struct ResponseBuilder {
    label: Option<String>,
    data: Option<Value>,
    path: Option<Path>,
    errors: Vec<graphql::Error>,
    extensions: ObjectAccumulator,
    status_code: Option<StatusCode>,
    headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
    context: Option<Context>,
}

impl ResponseBuilder {
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn and_label(mut self, label: Option<impl Into<String>>) -> Self {
        self.label = label.map(Into::into);
        self
    }

    pub fn data(mut self, data: impl Into<Value>) -> Self {
        self.data = Some(data.into());
        self
    }

    pub fn and_data(mut self, data: Option<impl Into<Value>>) -> Self {
        self.data = data.map(Into::into);
        self
    }

    pub fn path(mut self, path: impl Into<Path>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn and_path(mut self, path: Option<impl Into<Path>>) -> Self {
        self.path = path.map(Into::into);
        self
    }

    pub fn errors(mut self, errors: Vec<graphql::Error>) -> Self {
        self.errors.extend(errors);
        self
    }

    pub fn error(mut self, error: impl Into<graphql::Error>) -> Self {
        self.errors.push(error.into());
        self
    }

    /// Adds every member of the object-shaped `extensions` to the GraphQL extensions.
    pub fn extensions(mut self, extensions: Value) -> Self {
        self.extensions.extend(extensions);
        self
    }

    pub fn extension(mut self, key: impl Into<String>, value: impl Into<NewValue>) -> Self {
        self.extensions.insert(key, value);
        self
    }

    pub fn status_code(mut self, status_code: impl Into<StatusCode>) -> Self {
        self.status_code = Some(status_code.into());
        self
    }

    pub fn and_status_code(mut self, status_code: Option<impl Into<StatusCode>>) -> Self {
        self.status_code = status_code.map(Into::into);
        self
    }

    pub fn headers(mut self, headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>) -> Self {
        self.headers.extend(headers);
        self
    }

    pub fn header(
        mut self,
        name: impl Into<TryIntoHeaderName>,
        value: impl Into<TryIntoHeaderValue>,
    ) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    pub fn context(mut self, context: impl Into<Context>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// # Panics
    ///
    /// Panics unless `context` has been set.
    pub fn build(self) -> Result<Response, BoxError> {
        Response::new(
            self.label,
            self.data,
            self.path,
            self.errors,
            self.extensions.build(),
            self.status_code,
            self.headers,
            self.context.expect("context is required"),
        )
    }
}

/// Builds a [`Response`] with test-friendly defaults. Created by [`Response::fake_builder`].
#[derive(Default)]
pub struct FakeResponseBuilder {
    label: Option<String>,
    data: Option<Value>,
    path: Option<Path>,
    errors: Vec<graphql::Error>,
    extensions: ObjectAccumulator,
    status_code: Option<StatusCode>,
    headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
    context: Option<Context>,
}

impl FakeResponseBuilder {
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn and_label(mut self, label: Option<impl Into<String>>) -> Self {
        self.label = label.map(Into::into);
        self
    }

    pub fn data(mut self, data: impl Into<Value>) -> Self {
        self.data = Some(data.into());
        self
    }

    pub fn and_data(mut self, data: Option<impl Into<Value>>) -> Self {
        self.data = data.map(Into::into);
        self
    }

    pub fn path(mut self, path: impl Into<Path>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn and_path(mut self, path: Option<impl Into<Path>>) -> Self {
        self.path = path.map(Into::into);
        self
    }

    pub fn errors(mut self, errors: Vec<graphql::Error>) -> Self {
        self.errors.extend(errors);
        self
    }

    pub fn error(mut self, error: impl Into<graphql::Error>) -> Self {
        self.errors.push(error.into());
        self
    }

    /// Adds every member of the object-shaped `extensions` to the GraphQL extensions.
    pub fn extensions(mut self, extensions: Value) -> Self {
        self.extensions.extend(extensions);
        self
    }

    pub fn extension(mut self, key: impl Into<String>, value: impl Into<NewValue>) -> Self {
        self.extensions.insert(key, value);
        self
    }

    pub fn status_code(mut self, status_code: impl Into<StatusCode>) -> Self {
        self.status_code = Some(status_code.into());
        self
    }

    pub fn and_status_code(mut self, status_code: Option<impl Into<StatusCode>>) -> Self {
        self.status_code = status_code.map(Into::into);
        self
    }

    pub fn headers(mut self, headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>) -> Self {
        self.headers.extend(headers);
        self
    }

    pub fn header(
        mut self,
        name: impl Into<TryIntoHeaderName>,
        value: impl Into<TryIntoHeaderValue>,
    ) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    pub fn context(mut self, context: impl Into<Context>) -> Self {
        self.context = Some(context.into());
        self
    }

    pub fn and_context(mut self, context: Option<impl Into<Context>>) -> Self {
        self.context = context.map(Into::into);
        self
    }

    pub fn build(self) -> Result<Response, BoxError> {
        Response::fake_new(
            self.label,
            self.data,
            self.path,
            self.errors,
            self.extensions.build(),
            self.status_code,
            self.headers,
            self.context,
        )
    }
}

/// Builds a [`Response`] around an already-encoded HTTP response. Created by
/// [`Response::http_response_builder`].
#[derive(Default)]
pub struct HttpResponseBuilder {
    response: Option<http::Response<Body>>,
    context: Option<Context>,
    body_to_stash: Option<String>,
    errors_for_context: Option<Vec<graphql::Error>>,
}

impl HttpResponseBuilder {
    pub fn response(mut self, response: http::Response<Body>) -> Self {
        self.response = Some(response);
        self
    }

    pub fn context(mut self, context: impl Into<Context>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// Records `body_to_stash` on the context so telemetry can read the response body
    /// without consuming it.
    pub fn body_to_stash(mut self, body_to_stash: impl Into<String>) -> Self {
        self.body_to_stash = Some(body_to_stash.into());
        self
    }

    pub fn and_body_to_stash(mut self, body_to_stash: Option<impl Into<String>>) -> Self {
        self.body_to_stash = body_to_stash.map(Into::into);
        self
    }

    /// Records `errors_for_context` on the context for error counting, for responses whose
    /// body is not deserialized.
    pub fn errors_for_context(mut self, errors_for_context: Vec<graphql::Error>) -> Self {
        self.errors_for_context = Some(errors_for_context);
        self
    }

    pub fn and_errors_for_context(
        mut self,
        errors_for_context: Option<Vec<graphql::Error>>,
    ) -> Self {
        self.errors_for_context = errors_for_context;
        self
    }

    /// # Panics
    ///
    /// Panics unless `response` and `context` have both been set.
    pub fn build(self) -> Result<Response, BoxError> {
        Response::http_response_new(
            self.response.expect("response is required"),
            self.context.expect("context is required"),
            self.body_to_stash,
            self.errors_for_context,
        )
    }
}

/// Builds a [`Response`] carrying only errors. Created by [`Response::error_builder`].
#[derive(Default)]
pub struct ErrorResponseBuilder {
    errors: Vec<graphql::Error>,
    status_code: Option<StatusCode>,
    headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
    context: Option<Context>,
}

impl ErrorResponseBuilder {
    pub fn errors(mut self, errors: Vec<graphql::Error>) -> Self {
        self.errors.extend(errors);
        self
    }

    pub fn error(mut self, error: impl Into<graphql::Error>) -> Self {
        self.errors.push(error.into());
        self
    }

    pub fn status_code(mut self, status_code: impl Into<StatusCode>) -> Self {
        self.status_code = Some(status_code.into());
        self
    }

    pub fn and_status_code(mut self, status_code: Option<impl Into<StatusCode>>) -> Self {
        self.status_code = status_code.map(Into::into);
        self
    }

    pub fn headers(mut self, headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>) -> Self {
        self.headers.extend(headers);
        self
    }

    pub fn header(
        mut self,
        name: impl Into<TryIntoHeaderName>,
        value: impl Into<TryIntoHeaderValue>,
    ) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    pub fn context(mut self, context: impl Into<Context>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// # Panics
    ///
    /// Panics unless `context` has been set.
    pub fn build(self) -> Result<Response, BoxError> {
        Response::error_new(
            self.errors,
            self.status_code,
            self.headers,
            self.context.expect("context is required"),
        )
    }
}

/// Builds a [`Response`] from already-valid headers. Created by
/// [`Response::infallible_builder`].
#[derive(Default)]
pub(crate) struct InfallibleResponseBuilder {
    label: Option<String>,
    data: Option<Value>,
    path: Option<Path>,
    errors: Vec<graphql::Error>,
    extensions: ObjectAccumulator,
    status_code: Option<StatusCode>,
    headers: MultiMap<HeaderName, HeaderValue>,
    context: Option<Context>,
}

impl InfallibleResponseBuilder {
    pub(crate) fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub(crate) fn and_label(mut self, label: Option<impl Into<String>>) -> Self {
        self.label = label.map(Into::into);
        self
    }

    pub(crate) fn data(mut self, data: impl Into<Value>) -> Self {
        self.data = Some(data.into());
        self
    }

    pub(crate) fn and_data(mut self, data: Option<impl Into<Value>>) -> Self {
        self.data = data.map(Into::into);
        self
    }

    pub(crate) fn path(mut self, path: impl Into<Path>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub(crate) fn and_path(mut self, path: Option<impl Into<Path>>) -> Self {
        self.path = path.map(Into::into);
        self
    }

    pub(crate) fn errors(mut self, errors: Vec<graphql::Error>) -> Self {
        self.errors.extend(errors);
        self
    }

    pub(crate) fn error(mut self, error: impl Into<graphql::Error>) -> Self {
        self.errors.push(error.into());
        self
    }

    /// Adds every member of the object-shaped `extensions` to the GraphQL extensions.
    pub(crate) fn extensions(mut self, extensions: Value) -> Self {
        self.extensions.extend(extensions);
        self
    }

    pub(crate) fn extension(mut self, key: impl Into<String>, value: impl Into<NewValue>) -> Self {
        self.extensions.insert(key, value);
        self
    }

    pub(crate) fn status_code(mut self, status_code: impl Into<StatusCode>) -> Self {
        self.status_code = Some(status_code.into());
        self
    }

    pub(crate) fn and_status_code(mut self, status_code: Option<impl Into<StatusCode>>) -> Self {
        self.status_code = status_code.map(Into::into);
        self
    }

    pub(crate) fn headers(mut self, headers: MultiMap<HeaderName, HeaderValue>) -> Self {
        self.headers.extend(headers);
        self
    }

    pub(crate) fn header(
        mut self,
        name: impl Into<HeaderName>,
        value: impl Into<HeaderValue>,
    ) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    pub(crate) fn context(mut self, context: impl Into<Context>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// # Panics
    ///
    /// Panics unless `context` has been set.
    pub(crate) fn build(self) -> Response {
        Response::infallible_new(
            self.label,
            self.data,
            self.path,
            self.errors,
            self.extensions.build(),
            self.status_code,
            self.headers,
            self.context.expect("context is required"),
        )
    }
}

#[derive(Clone, Default, Debug)]
pub(crate) struct ClientRequestAccepts {
    pub(crate) multipart_defer: bool,
    pub(crate) multipart_subscription: bool,
    pub(crate) json: bool,
    pub(crate) wildcard: bool,
}

impl<T> From<http::Response<T>> for Response
where
    T: http_body::Body<Data = Bytes> + Send + 'static,
    <T as http_body::Body>::Error: Into<BoxError>,
{
    fn from(response: http::Response<T>) -> Self {
        let context: Context = response.extensions().get().cloned().unwrap_or_default();

        Self {
            response: response.map(convert_to_body),
            context,
        }
    }
}

impl From<Response> for http::Response<Body> {
    fn from(mut response: Response) -> Self {
        response.response.extensions_mut().insert(response.context);
        response.response
    }
}

impl<T> From<http::Request<T>> for Request
where
    T: http_body::Body<Data = Bytes> + Send + 'static,
    <T as http_body::Body>::Error: Into<BoxError>,
{
    fn from(request: http::Request<T>) -> Self {
        let context: Context = request.extensions().get().cloned().unwrap_or_default();

        Self {
            router_request: request.map(convert_to_body),
            context,
        }
    }
}

impl From<Request> for http::Request<Body> {
    fn from(mut request: Request) -> Self {
        request
            .router_request
            .extensions_mut()
            .insert(request.context);
        request.router_request
    }
}

/// This function is used to convert an `http_body::Body` into a `Body`.
/// It does a downcast check to see if the body is already a `Body` and if it is, then it just returns it.
/// There is zero overhead if the body is already a `Body`.
/// Note that ALL graphql responses are already a stream as they may be part of a deferred or stream response,
/// therefore, if a body has to be wrapped, the cost is minimal.
fn convert_to_body<T>(mut b: T) -> Body
where
    T: http_body::Body<Data = Bytes> + Send + 'static,
    <T as http_body::Body>::Error: Into<BoxError>,
{
    let val_any = &mut b as &mut dyn Any;
    match val_any.downcast_mut::<Body>() {
        Some(body) => mem::take(body),
        None => Body::new(http_body_util::BodyStream::new(b.map_err(axum::Error::new))),
    }
}

#[cfg(test)]
mod test {
    use std::pin::Pin;
    use std::task::Context;
    use std::task::Poll;

    use tower::BoxError;

    use super::convert_to_body;
    use crate::services::router;

    struct MockBody {
        data: Option<&'static str>,
    }
    impl http_body::Body for MockBody {
        type Data = bytes::Bytes;
        type Error = BoxError;

        fn poll_frame(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
            if let Some(data) = self.get_mut().data.take() {
                Poll::Ready(Some(Ok(http_body::Frame::data(bytes::Bytes::from(data)))))
            } else {
                Poll::Ready(None)
            }
        }
    }

    #[tokio::test]
    async fn test_convert_from_http_body() {
        let body = convert_to_body(MockBody { data: Some("test") });
        assert_eq!(
            &String::from_utf8(router::body::into_bytes(body).await.unwrap().to_vec()).unwrap(),
            "test"
        );
    }

    #[tokio::test]
    async fn test_convert_from_hyper_body() {
        let body = convert_to_body(String::from("test"));
        assert_eq!(
            &String::from_utf8(router::body::into_bytes(body).await.unwrap().to_vec()).unwrap(),
            "test"
        );
    }
}
