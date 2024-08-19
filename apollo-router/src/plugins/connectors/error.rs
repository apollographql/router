//! Connectors error types.

use apollo_federation::sources::connect::Connector;

use crate::graphql;
use crate::graphql::ErrorExtension;
use crate::json_ext::Path;

/// Errors that apply to all connector types. These errors represent a problem invoking the
/// connector, as opposed to an error returned from the connector itself.
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub(crate) enum Error {
    /// Request limit exceeded
    RequestLimitExceeded,
}

impl Error {
    /// Create a GraphQL error from this error.
    #[must_use]
    pub(crate) fn to_graphql_error(
        &self,
        connector: &Connector,
        path: Option<Path>,
    ) -> crate::error::Error {
        let builder = graphql::Error::builder()
            .message(self.to_string())
            .extension_code(self.extension_code())
            .extension("service", connector.id.label.clone());
        if let Some(path) = path {
            builder.path(path).build()
        } else {
            builder.build()
        }
    }
}

impl ErrorExtension for Error {
    fn extension_code(&self) -> String {
        match self {
            Self::RequestLimitExceeded => "REQUEST_LIMIT_EXCEEDED",
        }
        .to_string()
    }
}
