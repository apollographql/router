//! Error types

use std::error;
use std::fmt;

use crate::graphql;

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

impl From<RateLimited> for graphql::Error {
    fn from(_: RateLimited) -> Self {
        graphql::Error::builder()
            .message(String::from("Your request has been rate limited"))
            .extension_code("REQUEST_RATE_LIMITED")
            .build()
    }
}

impl error::Error for RateLimited {}
