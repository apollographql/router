#![allow(missing_docs)] // FIXME

use bytes::Bytes;
use futures::StreamExt;
use http::header::HeaderName;
use http::HeaderValue;
use http::StatusCode;
use multimap::MultiMap;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map as JsonMap;
use serde_json_bytes::Value;
use static_assertions::assert_impl_all;
use tower::BoxError;

use super::supergraph;
use crate::error::Error;
use crate::graphql;
use crate::json_ext::Path;
use crate::Context;
use crate::TryIntoHeaderName;
use crate::TryIntoHeaderValue;

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

impl TryFrom<supergraph::Request> for Request {
    type Error = ();
    fn try_from(request: supergraph::Request) -> Result<Self, Self::Error> {
        // TODO: handle errors
        Ok(Self {
            router_request: request
                .supergraph_request
                .map(|req| hyper::Body::from(serde_json::to_vec(&req).unwrap())),
            context: request.context,
        })
    }
}

assert_impl_all!(Response: Send);
#[non_exhaustive]
#[derive(Debug)]
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

impl From<supergraph::Response> for Response {
    fn from(supergraph_response: supergraph::Response) -> Self {
        let context = supergraph_response.context;
        let (parts, http_body) = supergraph_response.response.into_parts();

        let body =
            hyper::Body::wrap_stream(http_body.map(|response| serde_json::to_vec(&response)));

        Self {
            response: http::Response::from_parts(parts, body),
            context,
        }
    }
}

#[buildstructor::buildstructor]
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

        // let response = builder.body(once(ready(res)).boxed())?;

        let response = builder.body(hyper::Body::from(serde_json::to_vec(&res)?))?;

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
