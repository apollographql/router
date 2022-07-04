//! Error types

use std::error;
use std::fmt;

use axum::response::IntoResponse;
use http::StatusCode;

/// The timeout elapsed.
#[derive(Debug, Default)]
pub(crate) struct Elapsed(pub(super) ());

impl Elapsed {
    /// Construct a new elapsed error
    pub(crate) fn new() -> Self {
        Elapsed(())
    }
}

impl fmt::Display for Elapsed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("request timed out")
    }
}

impl IntoResponse for Elapsed {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::REQUEST_TIMEOUT, self.to_string()).into_response()
    }
}

impl error::Error for Elapsed {}
