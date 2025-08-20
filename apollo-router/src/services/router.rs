#![allow(missing_docs)] // FIXME

use std::any::Any;
use std::mem;

use ahash::HashMap;
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
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JsonMap;
use serde_json_bytes::Value;
use static_assertions::assert_impl_all;
use thiserror::Error;
use tower::BoxError;
use uuid::Uuid;

use self::body::RouterBody;
use self::service::MULTIPART_DEFER_CONTENT_TYPE_HEADER_VALUE;
use self::service::MULTIPART_SUBSCRIPTION_CONTENT_TYPE_HEADER_VALUE;
use super::supergraph;
use crate::Context;
use crate::context::CONTAINS_GRAPHQL_ERROR;
use crate::context::ROUTER_RESPONSE_ERRORS;
use crate::graphql;
use crate::http_ext::header_map;
use crate::json_ext::Path;
use crate::plugins::telemetry::config_new::router::events::RouterResponseBodyExtensionType;
use crate::services::TryIntoHeaderName;
use crate::services::TryIntoHeaderValue;

pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
pub type ServiceResult = Result<Response, BoxError>;

pub type Body = RouterBody;
pub type Error = hyper::Error;

pub mod body;
pub(crate) mod pipeline_handle;
pub(crate) mod service;
#[cfg(test)]
mod tests;

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

#[buildstructor::buildstructor]
impl Request {
    /// This is the constructor (or builder) to use when constructing a real Request.
    ///
    /// Required parameters are required in non-testing code to create a Request.
    #[builder(visibility = "pub")]
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

    /// This is the constructor (or builder) to use when constructing a fake Request.
    ///
    /// Required parameters are required in non-testing code to create a Request.
    #[builder(visibility = "pub")]
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

#[buildstructor::buildstructor]
impl Response {
    fn stash_the_body_in_extensions(&mut self, body_string: String) {
        self.context.extensions().with_lock(|ext| {
            ext.insert(RouterResponseBodyExtensionType(body_string));
        });
    }

    pub async fn next_response(&mut self) -> Option<Result<Bytes, axum::Error>> {
        self.response.body_mut().into_data_stream().next().await
    }

    /// This is the constructor (or builder) to use when constructing a real Response.
    ///
    /// Required parameters are required in non-testing code to create a Response.
    #[builder(visibility = "pub")]
    fn new(
        label: Option<String>,
        data: Option<serde_json_bytes::Value>,
        path: Option<Path>,
        errors: Vec<graphql::Error>,
        // Skip the `Object` type alias to use buildstructor’s map special-casing
        extensions: JsonMap<ByteString, serde_json_bytes::Value>,
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

    #[builder(visibility = "pub")]
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

    /// This is the constructor (or builder) to use when constructing a Response that represents a global error.
    /// It has no path and no response data.
    /// This is useful for things such as authentication errors.
    #[builder(visibility = "pub")]
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
            Default::default(),
            status_code,
            headers,
            context,
        )
    }

    /// This is the constructor (or builder) to use when constructing a real Response.
    ///
    /// Required parameters are required in non-testing code to create a Response.
    #[builder(visibility = "pub(crate)")]
    fn infallible_new(
        label: Option<String>,
        data: Option<serde_json_bytes::Value>,
        path: Option<Path>,
        errors: Vec<graphql::Error>,
        // Skip the `Object` type alias to use buildstructor’s map special-casing
        extensions: JsonMap<ByteString, serde_json_bytes::Value>,
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
        context.insert_json_value(CONTAINS_GRAPHQL_ERROR, Value::Bool(true));
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
    pub async fn into_graphql_response_stream(
        self,
    ) -> impl Stream<Item = Result<graphql::Response, serde_json::Error>> {
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
                        return Some((serde_json::from_slice::<graphql::Response>(&bytes), m));
                    }
                    None
                }))
            } else {
                let mut body = http_body_util::BodyDataStream::new(self.response.into_body());
                let res = body.next().await.and_then(|res| res.ok());

                Either::Right(
                    futures::stream::iter(res.into_iter())
                        .map(|bytes| serde_json::from_slice::<graphql::Response>(&bytes)),
                )
            },
        )
    }

    /// This is the constructor (or builder) to use when constructing a fake Response.
    ///
    /// Required parameters are required in non-testing code to create a Response.
    #[builder(visibility = "pub")]
    fn fake_new(
        label: Option<String>,
        data: Option<serde_json_bytes::Value>,
        path: Option<Path>,
        errors: Vec<graphql::Error>,
        // Skip the `Object` type alias to use buildstructor’s map special-casing
        extensions: JsonMap<ByteString, serde_json_bytes::Value>,
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
