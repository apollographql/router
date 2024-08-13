//! HTTP-based connector implementation types.

use hyper::body::HttpBody;

use crate::plugins::connectors::error::Error as ConnectorError;
use crate::plugins::connectors::make_requests::ResponseKey;

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
    pub(crate) result: Result<T>,
    pub(crate) key: ResponseKey,
}
