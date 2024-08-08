//! Connectors error types.

/// Errors that apply to all connector types. These errors represent a problem invoking the
/// connector, as opposed to an error returned from the connector itself.
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub(crate) enum Error {
    /// Request limit exceeded
    RequestLimitExceeded,
}
