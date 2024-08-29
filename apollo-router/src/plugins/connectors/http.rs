//! HTTP-based connector implementation types.

use http::Uri;
use hyper::body::HttpBody;

use super::plugin::ConnectorDebugHttpRequest;
use crate::plugins::connectors::error::Error as ConnectorError;
use crate::plugins::connectors::make_requests::ResponseKey;
use crate::services::router::body::RouterBody;

/// A result of a connector
pub(crate) enum Result<T: HttpBody> {
    /// The connector was not invoked because of an error
    Err(ConnectorError),

    /// The connector was invoked and returned an HTTP response
    HttpResponse(http::Response<T>),
}

impl<T: HttpBody> From<http::Response<T>> for Result<T> {
    fn from(value: http::Response<T>) -> Self {
        Result::HttpResponse(value)
    }
}

impl<T: HttpBody> From<ConnectorError> for Result<T> {
    fn from(value: ConnectorError) -> Self {
        Result::Err(value)
    }
}

/// The result of a connector and the associated response key
pub(crate) struct Response<T: HttpBody> {
    pub(crate) url: Option<Uri>,
    pub(crate) result: Result<T>,
    pub(crate) key: ResponseKey,
    pub(crate) debug_request: Option<ConnectorDebugHttpRequest>,
}

#[derive(Debug)]
pub(crate) struct Request {
    pub(crate) request: http::Request<RouterBody>,
    pub(crate) key: ResponseKey,
    pub(crate) debug_request: Option<ConnectorDebugHttpRequest>,
}
