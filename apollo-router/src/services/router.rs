#![allow(missing_docs)] // FIXME

use bytes::Bytes;
use futures::StreamExt;
use http::Method;
use static_assertions::assert_impl_all;
use tower::BoxError;

use super::supergraph;
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

            http::Request::from_parts(parts, hyper::Body::empty())
        } else {
            http::Request::from_parts(
                parts,
                hyper::Body::from(
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
pub struct Response {
    pub response: http::Response<hyper::Body>,
    pub context: Context,
}

impl From<http::Response<hyper::Body>> for Response {
    fn from(response: http::Response<hyper::Body>) -> Self {
        Self {
            response,
            context: Context::new(),
        }
    }
}

impl Response {
    pub async fn next_response(&mut self) -> Option<Result<Bytes, hyper::Error>> {
        self.response.body_mut().next().await
    }

    pub fn map<F>(self, f: F) -> Response
    where
        F: FnOnce(hyper::Body) -> hyper::Body,
    {
        Response {
            context: self.context,
            response: self.response.map(f),
        }
    }
}

// TODO[igni]: have a router::Request and router::Response equivalent eventually
// #[cfg(test)]
// mod test {
//     use http::HeaderValue;
//     use http::Method;
//     use http::Uri;
//     use serde_json::json;

//     use crate::graphql;
//     use crate::services::supergraph;
//     use crate::Context;

//     #[test]
//     fn router_request_builder() {
//         let request = supergraph::Request::builder()
//             .header("a", "b")
//             .header("a", "c")
//             .uri(Uri::from_static("http://example.com"))
//             .method(Method::POST)
//             .query("query { topProducts }")
//             .operation_name("Default")
//             .context(Context::new())
//             // We need to follow up on this. How can users creat this easily?
//             .extension("foo", json!({}))
//             // We need to follow up on this. How can users creat this easily?
//             .variable("bar", json!({}))
//             .build()
//             .unwrap();
//         assert_eq!(
//             request
//                 .supergraph_request
//                 .headers()
//                 .get_all("a")
//                 .into_iter()
//                 .collect::<Vec<_>>(),
//             vec![HeaderValue::from_static("b"), HeaderValue::from_static("c")]
//         );
//         assert_eq!(
//             request.supergraph_request.uri(),
//             &Uri::from_static("http://example.com")
//         );
//         assert_eq!(
//             request.supergraph_request.body().extensions.get("foo"),
//             Some(&json!({}).into())
//         );
//         assert_eq!(
//             request.supergraph_request.body().variables.get("bar"),
//             Some(&json!({}).into())
//         );
//         assert_eq!(request.supergraph_request.method(), Method::POST);

//         let extensions = serde_json_bytes::Value::from(json!({"foo":{}}))
//             .as_object()
//             .unwrap()
//             .clone();

//         let variables = serde_json_bytes::Value::from(json!({"bar":{}}))
//             .as_object()
//             .unwrap()
//             .clone();
//         assert_eq!(
//             request.supergraph_request.body(),
//             &hyper::Body::builder()
//                 .variables(variables)
//                 .extensions(extensions)
//                 .operation_name("Default")
//                 .query("query { topProducts }")
//                 .build()
//         );
//     }

//     #[tokio::test]
//     async fn router_response_builder() {
//         let mut response = supergraph::Response::builder()
//             .header("a", "b")
//             .header("a", "c")
//             .context(Context::new())
//             .extension("foo", json!({}))
//             .data(json!({}))
//             .build()
//             .unwrap();

//         assert_eq!(
//             response
//                 .response
//                 .headers()
//                 .get_all("a")
//                 .into_iter()
//                 .collect::<Vec<_>>(),
//             vec![HeaderValue::from_static("b"), HeaderValue::from_static("c")]
//         );
//         let extensions = serde_json_bytes::Value::from(json!({"foo":{}}))
//             .as_object()
//             .unwrap()
//             .clone();
//         assert_eq!(
//             response.next_response().await.unwrap(),
//             graphql::Response::builder()
//                 .extensions(extensions)
//                 .data(json!({}))
//                 .build()
//         );
//     }
// }
