//! Error types

use std::error;
use std::fmt;

use crate::graphql;

/// The rate limit error.
#[derive(Debug, Default)]
pub(crate) struct TpsLimited;

impl TpsLimited {
    /// Construct a new TpsLimited error
    pub(crate) fn new() -> Self {
        TpsLimited {}
    }
}

impl fmt::Display for TpsLimited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // TODO: product-y words
        f.pad("TODO: product-specified words")
    }
}

impl From<TpsLimited> for graphql::Error {
    fn from(_: TpsLimited) -> Self {
        graphql::Error::builder()
            .message(String::from("TODO: product words"))
            // TODO: decide on extension code; product insight
            .extension_code("TPS_RATE_LIMITED")
            .build()
    }
}

impl error::Error for TpsLimited {}
