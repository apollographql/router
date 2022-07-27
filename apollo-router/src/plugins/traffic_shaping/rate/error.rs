//! Error types

use std::error;
use std::fmt;

use axum::response::IntoResponse;
use http::StatusCode;

/// The rate limit error.
#[derive(Debug, Default)]
pub(crate) struct RateLimited;

impl RateLimited {
    /// Construct a new RateLimited error
    pub(crate) fn new() -> Self {
        RateLimited {}
    }
}

impl fmt::Display for RateLimited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("your request has been rate limited")
    }
}

impl IntoResponse for RateLimited {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::TOO_MANY_REQUESTS, self.to_string()).into_response()
    }
}

impl error::Error for RateLimited {}
