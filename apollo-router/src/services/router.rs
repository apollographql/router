#![allow(missing_docs)] // FIXME

use axum::body::BoxBody;
use futures::future::ready;
use futures::stream::once;
use futures::stream::StreamExt;
use http::header::HeaderName;
use http::method::Method;
use http::HeaderValue;
use http::StatusCode;
use http::Uri;
use hyper::Body;
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
    pub router_request: http::Request<hyper::Body>,

    /// Context for extension
    pub context: Context,
}

impl From<http::Request<hyper::Body>> for Request {
    fn from(router_request: http::Request<hyper::Body>) -> Self {
        Self {
            router_request,
            context: Context::new(),
        }
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
        let gql_request = hyper::Body::builder()
            .and_query(query)
            .and_operation_name(operation_name)
            .variables(variables)
            .extensions(extensions)
            .build();
        let mut router_request = http::Request::builder()
            .uri(uri)
            .method(method)
            .body(gql_request)?;
        *router_request.headers_mut() = header_map(headers)?;
        Ok(Self {
            router_request,
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
            .or_insert(HeaderValue::from_static("application/json").into());
        Request::new(
            query,
            operation_name,
            variables,
            extensions,
            context.unwrap_or_default(),
            headers,
            Uri::from_static("http://default"),
            method.unwrap_or(Method::GET),
        )
    }

    /// Create a request with an example query, for tests
    #[builder(visibility = "pub")]
    fn canned_new(
        operation_name: Option<String>,
        // Skip the `Object` type alias in order to use buildstructor’s map special-casing
        extensions: JsonMap<ByteString, Value>,
        context: Option<Context>,
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
    ) -> Result<Request, BoxError> {
        let query = "
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
        let mut variables = JsonMap::new();
        variables.insert("first", 2_usize.into());
        Self::fake_new(
            Some(query.to_owned()),
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
    pub response: http::Response<Body>,
    pub context: Context,
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
    fn router_request_builder() {
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
                .router_request
                .headers()
                .get_all("a")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![HeaderValue::from_static("b"), HeaderValue::from_static("c")]
        );
        assert_eq!(
            request.router_request.uri(),
            &Uri::from_static("http://example.com")
        );
        assert_eq!(
            request.router_request.body().extensions.get("foo"),
            Some(&json!({}).into())
        );
        assert_eq!(
            request.router_request.body().variables.get("bar"),
            Some(&json!({}).into())
        );
        assert_eq!(request.router_request.method(), Method::POST);

        let extensions = serde_json_bytes::Value::from(json!({"foo":{}}))
            .as_object()
            .unwrap()
            .clone();

        let variables = serde_json_bytes::Value::from(json!({"bar":{}}))
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(
            request.router_request.body(),
            &hyper::Body::builder()
                .variables(variables)
                .extensions(extensions)
                .operation_name("Default")
                .query("query { topProducts }")
                .build()
        );
    }

    #[tokio::test]
    async fn router_response_builder() {
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
