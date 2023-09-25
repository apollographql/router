#![allow(missing_docs)] // FIXME

use bytes::Bytes;
use futures::future::Either;
use futures::Stream;
use futures::StreamExt;
use http::header::HeaderName;
use http::header::CONTENT_TYPE;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use multer::Multipart;
use multimap::MultiMap;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JsonMap;
use serde_json_bytes::Value;
use static_assertions::assert_impl_all;
use tower::BoxError;

use super::router_service::MULTIPART_DEFER_HEADER_VALUE;
use super::router_service::MULTIPART_SUBSCRIPTION_HEADER_VALUE;
use super::supergraph;
use crate::graphql;
use crate::json_ext::Path;
use crate::services::TryIntoHeaderName;
use crate::services::TryIntoHeaderValue;
use crate::Context;

pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
pub type ServiceResult = Result<Response, BoxError>;
pub type Body = hyper::Body;
pub type Error = hyper::Error;

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

impl From<http::Request<Body>> for Request {
    fn from(router_request: http::Request<Body>) -> Self {
        Self {
            router_request,
            context: Context::new(),
        }
    }
}

use displaydoc::Display;
use thiserror::Error;

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

            http::Request::from_parts(parts, Body::empty())
        } else {
            http::Request::from_parts(
                parts,
                Body::from(serde_json::to_vec(&request).map_err(ParseError::SerializationError)?),
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

impl From<http::Response<Body>> for Response {
    fn from(response: http::Response<Body>) -> Self {
        Self {
            response,
            context: Context::new(),
        }
    }
}

#[buildstructor::buildstructor]
impl Response {
    pub async fn next_response(&mut self) -> Option<Result<Bytes, Error>> {
        self.response.body_mut().next().await
    }

    pub fn map<F>(self, f: F) -> Response
    where
        F: FnOnce(Body) -> Body,
    {
        Response {
            context: self.context,
            response: self.response.map(f),
        }
    }

    /// This is the constructor (or builder) to use when constructing a real Response..
    ///
    /// Required parameters are required in non-testing code to create a Response..
    #[allow(clippy::too_many_arguments)]
    #[builder(visibility = "pub")]
    fn new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<graphql::Error>,
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

        // let response = builder.body(once(ready(res)).boxed())?;

        let response = builder.body(hyper::Body::from(serde_json::to_vec(&res)?))?;

        Ok(Self { response, context })
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

    /// EXPERIMENTAL: this is function is experimental and subject to potentially change.
    pub async fn into_graphql_response_stream(
        self,
    ) -> impl Stream<Item = Result<crate::graphql::Response, serde_json::Error>> {
        Box::pin(
            if self
                .response
                .headers()
                .get(CONTENT_TYPE)
                .iter()
                .any(|value| {
                    *value == MULTIPART_DEFER_HEADER_VALUE
                        || *value == MULTIPART_SUBSCRIPTION_HEADER_VALUE
                })
            {
                let multipart = Multipart::new(self.response.into_body(), "graphql");

                Either::Left(futures::stream::unfold(multipart, |mut m| async {
                    if let Ok(Some(response)) = m.next_field().await {
                        if let Ok(bytes) = response.bytes().await {
                            return Some((
                                serde_json::from_slice::<crate::graphql::Response>(&bytes),
                                m,
                            ));
                        }
                    }
                    None
                }))
            } else {
                let mut body = self.response.into_body();
                let res = body.next().await.and_then(|res| res.ok());

                Either::Right(
                    futures::stream::iter(res.into_iter())
                        .map(|bytes| serde_json::from_slice::<crate::graphql::Response>(&bytes)),
                )
            },
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
