//! Error types

use std::error;
use std::fmt;

use axum::response::IntoResponse;
use http::StatusCode;

use crate::json_ext::Object;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::Context;

/// The timeout elapsed.
#[derive(Debug, Default)]
pub(crate) struct Elapsed {
    context: Context,
}

impl Elapsed {
    /// Construct a new elapsed error
    pub(crate) fn new(context: Context) -> Self {
        Elapsed { context }
    }
}

impl fmt::Display for Elapsed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("request timed out")
    }
}

impl From<Elapsed> for supergraph::Response {
    fn from(e: Elapsed) -> Self {
        supergraph::Response::builder()
            .context(e.context)
            .status_code(StatusCode::REQUEST_TIMEOUT)
            .build()
            .expect("this should never fail")
    }
}

impl From<Elapsed> for subgraph::Response {
    fn from(e: Elapsed) -> Self {
        subgraph::Response::builder()
            .context(e.context)
            .status_code(StatusCode::GATEWAY_TIMEOUT)
            .extensions(Object::default())
            .build()
    }
}

impl IntoResponse for Elapsed {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::REQUEST_TIMEOUT, self.to_string()).into_response()
    }
}

impl error::Error for Elapsed {}
