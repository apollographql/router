#![allow(missing_docs)] // FIXME

use bytes::Bytes;
use futures::Stream;
use futures::StreamExt;
use http::Method;
use multer::Multipart;
use static_assertions::assert_impl_all;
use tower::BoxError;

use super::supergraph;
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
        } = request;

        let (mut parts, request) = supergraph_request.into_parts();

        let router_request = if parts.method == Method::GET {
            // get request
            let get_path = serde_urlencoded::to_string(&[
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

    /// EXPERIMENTAL: this is function is experimental and subject to potentially change.
    pub async fn into_graphql_response_stream(
        self,
    ) -> impl Stream<Item = Result<crate::graphql::Response, serde_json::Error>> {
        let multipart = Multipart::new(self.response.into_body(), "graphql");

        Box::pin(futures::stream::unfold(multipart, |mut m| async {
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
    }
}
