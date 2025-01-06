//! Connectors error types.

use apollo_federation::sources::connect::Connector;
use tower::BoxError;

use crate::graphql;
use crate::graphql::ErrorExtension;
use crate::json_ext::Path;

/// Errors that apply to all connector types. These errors represent a problem invoking the
/// connector, as opposed to an error returned from the connector itself.
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub(crate) enum Error {
    /// Request limit exceeded
    RequestLimitExceeded,

    /// {0}
    HTTPClientError(#[from] BoxError),
}

impl Error {
    /// Create a GraphQL error from this error.
    #[must_use]
    pub(crate) fn to_graphql_error(
        &self,
        connector: &Connector,
        path: Option<Path>,
    ) -> crate::error::Error {
        use serde_json_bytes::*;

        let builder = graphql::Error::builder()
            .message(self.to_string())
            .extension_code(self.extension_code())
            .extension("service", connector.id.subgraph_name.clone())
            .extension(
                "connector",
                Value::Object(Map::from_iter([(
                    "coordinate".into(),
                    Value::String(connector.id.coordinate().into()),
                )])),
            );
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
            Self::HTTPClientError(_) => "HTTP_CLIENT_ERROR",
        }
        .to_string()
    }
}
