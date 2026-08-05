#![allow(missing_docs)] // FIXME

use apollo_json::NewValue;
use futures::future::ready;
use futures::stream::StreamExt;
use futures::stream::once;
use http::HeaderValue;
use http::StatusCode;
use http::Uri;
use http::header::HeaderName;
use http::method::Method;
use mime::APPLICATION_JSON;
use multimap::MultiMap;
use static_assertions::assert_impl_all;
use tower::BoxError;

use crate::Context;
use crate::context::CHUNK_CONTAINS_GRAPHQL_ERROR;
use crate::context::CONTAINS_GRAPHQL_ERROR;
use crate::error::Error;
use crate::graphql;
use crate::graphql::json_object::ObjectAccumulator;
use crate::graphql::json_object::empty_object;
use crate::http_ext::TryIntoHeaderName;
use crate::http_ext::TryIntoHeaderValue;
use crate::http_ext::header_map;
use crate::json_ext::Path;
use crate::json_ext::Value;

pub(crate) mod service;
#[cfg(test)]
mod tests;

pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
pub type ServiceResult = Result<Response, BoxError>;

assert_impl_all!(Request: Send);
/// Represents the router processing step of the processing pipeline.
///
/// This consists of the parsed graphql Request, HTTP headers and contextual data for extensions.
#[non_exhaustive]
pub struct Request {
    /// Original request to the Router.
    pub supergraph_request: http::Request<graphql::Request>,

    /// Context for extension
    pub context: Context,
}

impl From<http::Request<graphql::Request>> for Request {
    fn from(supergraph_request: http::Request<graphql::Request>) -> Self {
        Self {
            supergraph_request,
            context: Context::new(),
        }
    }
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            // .field("supergraph_request", &self.supergraph_request)
            .field("context", &self.context)
            .finish()
    }
}

impl Request {
    /// Starts building a request for the supergraph service.
    ///
    /// `context`, `uri` and `method` have no sensible default and must be set before
    /// [`RequestBuilder::build`].
    pub fn builder() -> RequestBuilder {
        RequestBuilder::default()
    }

    /// Starts building a request with test-friendly defaults: an empty context, a
    /// `http://default` URI, `POST`, and a `content-type: application/json` header unless
    /// one is supplied.
    pub fn fake_builder() -> FakeRequestBuilder {
        FakeRequestBuilder::default()
    }

    /// Starts building a request carrying an example `TopProducts` query and its variables,
    /// on top of the [`Request::fake_builder`] defaults.
    pub fn canned_builder() -> CannedRequestBuilder {
        CannedRequestBuilder::default()
    }

    fn new(
        query: Option<String>,
        operation_name: Option<String>,
        variables: Value,
        extensions: Value,
        context: Context,
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
        uri: Uri,
        method: Method,
    ) -> Result<Request, BoxError> {
        let gql_request = graphql::Request::builder()
            .and_query(query)
            .and_operation_name(operation_name)
            .variables(variables)
            .extensions(extensions)
            .build();
        let mut supergraph_request = http::Request::builder()
            .uri(uri)
            .method(method)
            .body(gql_request)?;
        *supergraph_request.headers_mut() = header_map(headers)?;
        Ok(Self {
            supergraph_request,
            context,
        })
    }

    fn fake_new(
        query: Option<String>,
        operation_name: Option<String>,
        variables: Value,
        extensions: Value,
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
            query,
            operation_name,
            variables,
            extensions,
            context,
            headers,
            Uri::from_static("http://default"),
            method.unwrap_or(Method::POST),
        )
    }

    fn canned_new(
        query: Option<String>,
        operation_name: Option<String>,
        extensions: Value,
        context: Option<Context>,
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
    ) -> Result<Request, BoxError> {
        let default_query = "
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
            }
        ";
        let query = query.unwrap_or(default_query.to_string());
        Self::fake_new(
            Some(query),
            operation_name,
            Value::object([("first", 2_i64)]),
            extensions,
            context,
            headers,
            None,
        )
    }
}

/// Builds a [`Request`]. Created by [`Request::builder`].
#[derive(Default)]
pub struct RequestBuilder {
    query: Option<String>,
    operation_name: Option<String>,
    variables: ObjectAccumulator,
    extensions: ObjectAccumulator,
    context: Option<Context>,
    headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
    uri: Option<Uri>,
    method: Option<Method>,
}

impl RequestBuilder {
    pub fn query(mut self, query: impl Into<String>) -> Self {
        self.query = Some(query.into());
        self
    }

    pub fn and_query(mut self, query: Option<impl Into<String>>) -> Self {
        self.query = query.map(Into::into);
        self
    }

    pub fn operation_name(mut self, operation_name: impl Into<String>) -> Self {
        self.operation_name = Some(operation_name.into());
        self
    }

    pub fn and_operation_name(mut self, operation_name: Option<impl Into<String>>) -> Self {
        self.operation_name = operation_name.map(Into::into);
        self
    }

    /// Adds every member of the object-shaped `variables` to the GraphQL variables.
    pub fn variables(mut self, variables: Value) -> Self {
        self.variables.extend(variables);
        self
    }

    pub fn variable(mut self, key: impl Into<String>, value: impl Into<NewValue>) -> Self {
        self.variables.insert(key, value);
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

    pub fn uri(mut self, uri: impl Into<Uri>) -> Self {
        self.uri = Some(uri.into());
        self
    }

    pub fn method(mut self, method: impl Into<Method>) -> Self {
        self.method = Some(method.into());
        self
    }

    /// # Panics
    ///
    /// Panics unless `context`, `uri` and `method` have all been set.
    pub fn build(self) -> Result<Request, BoxError> {
        Request::new(
            self.query,
            self.operation_name,
            self.variables.build(),
            self.extensions.build(),
            self.context.expect("context is required"),
            self.headers,
            self.uri.expect("uri is required"),
            self.method.expect("method is required"),
        )
    }
}

/// Builds a [`Request`] with test-friendly defaults. Created by [`Request::fake_builder`].
#[derive(Default)]
pub struct FakeRequestBuilder {
    query: Option<String>,
    operation_name: Option<String>,
    variables: ObjectAccumulator,
    extensions: ObjectAccumulator,
    context: Option<Context>,
    headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
    method: Option<Method>,
}

impl FakeRequestBuilder {
    pub fn query(mut self, query: impl Into<String>) -> Self {
        self.query = Some(query.into());
        self
    }

    pub fn and_query(mut self, query: Option<impl Into<String>>) -> Self {
        self.query = query.map(Into::into);
        self
    }

    pub fn operation_name(mut self, operation_name: impl Into<String>) -> Self {
        self.operation_name = Some(operation_name.into());
        self
    }

    pub fn and_operation_name(mut self, operation_name: Option<impl Into<String>>) -> Self {
        self.operation_name = operation_name.map(Into::into);
        self
    }

    /// Adds every member of the object-shaped `variables` to the GraphQL variables.
    pub fn variables(mut self, variables: Value) -> Self {
        self.variables.extend(variables);
        self
    }

    pub fn variable(mut self, key: impl Into<String>, value: impl Into<NewValue>) -> Self {
        self.variables.insert(key, value);
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

    pub fn method(mut self, method: impl Into<Method>) -> Self {
        self.method = Some(method.into());
        self
    }

    pub fn build(self) -> Result<Request, BoxError> {
        Request::fake_new(
            self.query,
            self.operation_name,
            self.variables.build(),
            self.extensions.build(),
            self.context,
            self.headers,
            self.method,
        )
    }
}

/// Builds a [`Request`] carrying an example query. Created by [`Request::canned_builder`].
#[derive(Default)]
pub struct CannedRequestBuilder {
    query: Option<String>,
    operation_name: Option<String>,
    extensions: ObjectAccumulator,
    context: Option<Context>,
    headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
}

impl CannedRequestBuilder {
    pub fn query(mut self, query: impl Into<String>) -> Self {
        self.query = Some(query.into());
        self
    }

    pub fn operation_name(mut self, operation_name: impl Into<String>) -> Self {
        self.operation_name = Some(operation_name.into());
        self
    }

    pub fn and_operation_name(mut self, operation_name: Option<impl Into<String>>) -> Self {
        self.operation_name = operation_name.map(Into::into);
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

    pub fn context(mut self, context: impl Into<Context>) -> Self {
        self.context = Some(context.into());
        self
    }

    pub fn and_context(mut self, context: Option<impl Into<Context>>) -> Self {
        self.context = context.map(Into::into);
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

    pub fn build(self) -> Result<Request, BoxError> {
        Request::canned_new(
            self.query,
            self.operation_name,
            self.extensions.build(),
            self.context,
            self.headers,
        )
    }
}

assert_impl_all!(Response: Send);
#[non_exhaustive]
pub struct Response {
    pub response: http::Response<graphql::ResponseStream>,
    pub context: Context,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("context", &self.context)
            .finish()
    }
}

impl Response {
    /// Starts building a response for the supergraph service, carrying a single GraphQL
    /// response.
    ///
    /// `context` has no sensible default and must be set before [`ResponseBuilder::build`].
    /// Supplying any error also records the GraphQL-error flags on the context.
    pub fn builder() -> ResponseBuilder {
        ResponseBuilder::default()
    }

    /// Starts building a response with test-friendly defaults: an empty context and a
    /// `200 OK` status.
    pub fn fake_builder() -> FakeResponseBuilder {
        FakeResponseBuilder::default()
    }

    /// Starts building a response whose body is the supplied sequence of GraphQL responses,
    /// as a `@defer` or subscription stream produces.
    ///
    /// `context` has no sensible default and must be set before
    /// [`FakeStreamResponseBuilder::build`].
    pub fn fake_stream_builder() -> FakeStreamResponseBuilder {
        FakeStreamResponseBuilder::default()
    }

    /// Starts building a response carrying only errors, with no data and no path — the shape
    /// a request-level rejection such as an authentication failure takes.
    ///
    /// `context` has no sensible default and must be set before
    /// [`ErrorResponseBuilder::build`].
    pub fn error_builder() -> ErrorResponseBuilder {
        ErrorResponseBuilder::default()
    }

    /// Starts building a response from headers that are already valid, so that building
    /// cannot fail.
    ///
    /// `context` has no sensible default and must be set before
    /// [`InfallibleResponseBuilder::build`].
    pub(crate) fn infallible_builder() -> InfallibleResponseBuilder {
        InfallibleResponseBuilder::default()
    }

    fn new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
        extensions: Value,
        status_code: Option<StatusCode>,
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
        context: Context,
    ) -> Result<Self, BoxError> {
        let has_errors = !errors.is_empty();
        if has_errors {
            context.insert_json_value(CONTAINS_GRAPHQL_ERROR, Value::from(true));
        }
        context.insert_json_value(CHUNK_CONTAINS_GRAPHQL_ERROR, Value::from(has_errors));
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
        for (key, values) in headers {
            let header_name: HeaderName = key.try_into()?;
            for value in values {
                let header_value: HeaderValue = value.try_into()?;
                builder = builder.header(header_name.clone(), header_value);
            }
        }

        let response = builder.body(once(ready(res)).boxed())?;

        Ok(Self { response, context })
    }

    fn fake_new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
        extensions: Value,
        status_code: Option<StatusCode>,
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
        context: Option<Context>,
    ) -> Result<Self, BoxError> {
        Response::new(
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

    fn fake_stream_new(
        responses: Vec<graphql::Response>,
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

    fn error_new(
        errors: Vec<Error>,
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
        errors: Vec<Error>,
        extensions: Value,
        status_code: Option<StatusCode>,
        headers: MultiMap<HeaderName, HeaderValue>,
        context: Context,
    ) -> Self {
        let has_errors = !errors.is_empty();
        if has_errors {
            context.insert_json_value(CONTAINS_GRAPHQL_ERROR, Value::from(true));
        }
        context.insert_json_value(CHUNK_CONTAINS_GRAPHQL_ERROR, Value::from(has_errors));
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

        let response = builder.body(once(ready(res)).boxed()).expect("can't fail");

        Self { response, context }
    }

    pub(crate) fn new_from_graphql_response(response: graphql::Response, context: Context) -> Self {
        let has_errors = !response.errors.is_empty();
        if has_errors {
            context.insert_json_value(CONTAINS_GRAPHQL_ERROR, Value::from(true));
        }
        context.insert_json_value(CHUNK_CONTAINS_GRAPHQL_ERROR, Value::from(has_errors));

        Self {
            response: http::Response::new(once(ready(response)).boxed()),
            context,
        }
    }
}

/// Builds a [`Response`]. Created by [`Response::builder`].
#[derive(Default)]
pub struct ResponseBuilder {
    label: Option<String>,
    data: Option<Value>,
    path: Option<Path>,
    errors: Vec<Error>,
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

    pub fn errors(mut self, errors: Vec<Error>) -> Self {
        self.errors.extend(errors);
        self
    }

    pub fn error(mut self, error: impl Into<Error>) -> Self {
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
    errors: Vec<Error>,
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

    pub fn errors(mut self, errors: Vec<Error>) -> Self {
        self.errors.extend(errors);
        self
    }

    pub fn error(mut self, error: impl Into<Error>) -> Self {
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

/// Builds a [`Response`] whose body is a stream of GraphQL responses. Created by
/// [`Response::fake_stream_builder`].
#[derive(Default)]
pub struct FakeStreamResponseBuilder {
    responses: Vec<graphql::Response>,
    status_code: Option<StatusCode>,
    headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
    context: Option<Context>,
}

impl FakeStreamResponseBuilder {
    pub fn responses(mut self, responses: Vec<graphql::Response>) -> Self {
        self.responses.extend(responses);
        self
    }

    pub fn response(mut self, response: impl Into<graphql::Response>) -> Self {
        self.responses.push(response.into());
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
        Response::fake_stream_new(
            self.responses,
            self.status_code,
            self.headers,
            self.context.expect("context is required"),
        )
    }
}

/// Builds a [`Response`] carrying only errors. Created by [`Response::error_builder`].
#[derive(Default)]
pub struct ErrorResponseBuilder {
    errors: Vec<Error>,
    status_code: Option<StatusCode>,
    headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
    context: Option<Context>,
}

impl ErrorResponseBuilder {
    pub fn errors(mut self, errors: Vec<Error>) -> Self {
        self.errors.extend(errors);
        self
    }

    pub fn error(mut self, error: impl Into<Error>) -> Self {
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
    errors: Vec<Error>,
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

    pub(crate) fn errors(mut self, errors: Vec<Error>) -> Self {
        self.errors.extend(errors);
        self
    }

    pub(crate) fn error(mut self, error: impl Into<Error>) -> Self {
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

impl Response {
    pub async fn next_response(&mut self) -> Option<graphql::Response> {
        self.response.body_mut().next().await
    }

    pub(crate) fn new_from_response(
        response: http::Response<graphql::ResponseStream>,
        context: Context,
    ) -> Self {
        Self { context, response }.check_for_errors()
    }

    pub fn map<F>(self, f: F) -> Response
    where
        F: FnOnce(graphql::ResponseStream) -> graphql::ResponseStream,
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
    ///     fn supergraph_service(&self, inner: supergraph::BoxCloneService) -> supergraph::BoxCloneService {
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
        F: 'static + Send + FnMut(graphql::Response) -> graphql::Response,
    {
        self.map(move |stream| stream.map(f).boxed())
    }

    fn check_for_errors(self) -> Self {
        let context = self.context.clone();
        self.map_stream(move |response| {
            let has_errors = response.contains_errors();
            if has_errors {
                context.insert_json_value(CONTAINS_GRAPHQL_ERROR, Value::from(true));
            }
            context.insert_json_value(CHUNK_CONTAINS_GRAPHQL_ERROR, Value::from(has_errors));
            response
        })
    }
}

#[cfg(test)]
mod test {
    use http::HeaderValue;
    use http::Method;
    use http::Uri;
    use serde_json_bytes::json;

    use super::*;
    use crate::graphql;
    use crate::json_ext;

    /// A `json!` fixture in the apollo-json representation.
    fn value(fixture: serde_json_bytes::Value) -> Value {
        json_ext::from_legacy(&fixture)
    }

    #[test]
    fn supergraph_request_builder() {
        let request = Request::builder()
            .header("a", "b")
            .header("a", "c")
            .uri(Uri::from_static("http://example.com"))
            .method(Method::POST)
            .query("query { topProducts }")
            .operation_name("Default")
            .context(Context::new())
            // We need to follow up on this. How can users creat this easily?
            .extension("foo", value(json!({})))
            // We need to follow up on this. How can users creat this easily?
            .variable("bar", value(json!({})))
            .build()
            .unwrap();
        assert_eq!(
            request
                .supergraph_request
                .headers()
                .get_all("a")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![HeaderValue::from_static("b"), HeaderValue::from_static("c")]
        );
        assert_eq!(
            request.supergraph_request.uri(),
            &Uri::from_static("http://example.com")
        );
        assert_eq!(
            request.supergraph_request.body().extensions.get("foo"),
            Some(value(json!({})))
        );
        assert_eq!(
            request.supergraph_request.body().variables.get("bar"),
            Some(value(json!({})))
        );
        assert_eq!(request.supergraph_request.method(), Method::POST);

        assert_eq!(
            request.supergraph_request.body(),
            &graphql::Request::builder()
                .variables(value(json!({"bar":{}})))
                .extensions(value(json!({"foo":{}})))
                .operation_name("Default")
                .query("query { topProducts }")
                .build()
        );
    }

    #[tokio::test]
    async fn supergraph_response_builder() {
        let mut response = Response::builder()
            .header("a", "b")
            .header("a", "c")
            .context(Context::new())
            .extension("foo", value(json!({})))
            .data(value(json!({})))
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
        assert_eq!(
            response.next_response().await.unwrap(),
            graphql::Response::builder()
                .extensions(value(json!({"foo":{}})))
                .data(value(json!({})))
                .build()
        );
    }
}
