#![allow(missing_docs)] // FIXME

use futures::future::ready;
use futures::stream::once;
use futures::stream::StreamExt;
use http::header::HeaderName;
use http::method::Method;
use http::HeaderValue;
use http::StatusCode;
use http::Uri;
use mime::APPLICATION_JSON;
use multimap::MultiMap;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JsonMap;
use serde_json_bytes::Value;
use static_assertions::assert_impl_all;
use tower::BoxError;

use crate::error::Error;
use crate::graphql;
use crate::http_ext::header_map;
use crate::http_ext::TryIntoHeaderName;
use crate::http_ext::TryIntoHeaderValue;
use crate::json_ext::Path;
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

#[buildstructor::buildstructor]
impl Request {
    /// This is the constructor (or builder) to use when constructing a real Request.
    ///
    /// Required parameters are required in non-testing code to create a Request.
    #[allow(clippy::too_many_arguments)]
    #[builder(visibility = "pub")]
    fn new(
        query: Option<String>,
        operation_name: Option<String>,
        // Skip the `Object` type alias in order to use buildstructor’s map special-casing
        variables: JsonMap<ByteString, Value>,
        extensions: JsonMap<ByteString, Value>,
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

    /// This is the constructor (or builder) to use when constructing a "fake" Request.
    ///
    /// This does not enforce the provision of the data that is required for a fully functional
    /// Request. It's usually enough for testing, when a fully constructed Request is
    /// difficult to construct and not required for the purposes of the test.
    ///
    /// In addition, fake requests are expected to be valid, and will panic if given invalid values.
    #[builder(visibility = "pub")]
    fn fake_new(
        query: Option<String>,
        operation_name: Option<String>,
        // Skip the `Object` type alias in order to use buildstructor’s map special-casing
        variables: JsonMap<ByteString, Value>,
        extensions: JsonMap<ByteString, Value>,
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

    /// Create a request with an example query, for tests
    #[builder(visibility = "pub")]
    fn canned_new(
        query: Option<String>,
        operation_name: Option<String>,
        // Skip the `Object` type alias in order to use buildstructor’s map special-casing
        extensions: JsonMap<ByteString, Value>,
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
        let mut variables = JsonMap::new();
        variables.insert("first", 2_usize.into());
        Self::fake_new(
            Some(query),
            operation_name,
            variables,
            extensions,
            context,
            headers,
            None,
        )
    }
}

assert_impl_all!(Response: Send);
#[non_exhaustive]
pub struct Response {
    pub response: http::Response<graphql::ResponseStream>,
    pub context: Context,
}

#[buildstructor::buildstructor]
impl Response {
    /// This is the constructor (or builder) to use when constructing a real Response..
    ///
    /// Required parameters are required in non-testing code to create a Response..
    #[allow(clippy::too_many_arguments)]
    #[builder(visibility = "pub")]
    fn new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
        // Skip the `Object` type alias in order to use buildstructor’s map special-casing
        extensions: JsonMap<ByteString, Value>,
        status_code: Option<StatusCode>,
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
        context: Context,
    ) -> Result<Self, BoxError> {
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
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
        // Skip the `Object` type alias in order to use buildstructor’s map special-casing
        extensions: JsonMap<ByteString, Value>,
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

    /// This is the constructor (or builder) to use when constructing a "fake" Response stream.
    ///
    /// This does not enforce the provision of the data that is required for a fully functional
    /// Response. It's usually enough for testing, when a fully constructed Response is
    /// difficult to construct and not required for the purposes of the test.
    ///
    /// In addition, fake responses are expected to be valid, and will panic if given invalid values.
    #[builder(visibility = "pub")]
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

    /// This is the constructor (or builder) to use when constructing a Response that represents a global error.
    /// It has no path and no response data.
    /// This is useful for things such as authentication errors.
    #[builder(visibility = "pub")]
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
            Default::default(),
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
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
        // Skip the `Object` type alias in order to use buildstructor’s map special-casing
        extensions: JsonMap<ByteString, Value>,
        status_code: Option<StatusCode>,
        headers: MultiMap<HeaderName, HeaderValue>,
        context: Context,
    ) -> Self {
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
        Self {
            response: http::Response::new(once(ready(response)).boxed()),
            context,
        }
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
        Self { response, context }
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
        F: 'static + Send + FnMut(graphql::Response) -> graphql::Response,
    {
        self.map(move |stream| stream.map(f).boxed())
    }
}

#[cfg(test)]
mod test {
    use http::HeaderValue;
    use http::Method;
    use http::Uri;
    use serde_json::json;

    use super::*;
    use crate::graphql;

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
            .extension("foo", json!({}))
            // We need to follow up on this. How can users creat this easily?
            .variable("bar", json!({}))
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
            Some(&json!({}).into())
        );
        assert_eq!(
            request.supergraph_request.body().variables.get("bar"),
            Some(&json!({}).into())
        );
        assert_eq!(request.supergraph_request.method(), Method::POST);

        let extensions = serde_json_bytes::Value::from(json!({"foo":{}}))
            .as_object()
            .unwrap()
            .clone();

        let variables = serde_json_bytes::Value::from(json!({"bar":{}}))
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(
            request.supergraph_request.body(),
            &graphql::Request::builder()
                .variables(variables)
                .extensions(extensions)
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
