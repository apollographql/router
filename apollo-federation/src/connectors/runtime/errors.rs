use std::fmt::Display;

use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;

use crate::connectors::Connector;
use crate::connectors::runtime::key::ResponseKey;

#[derive(Debug)]
pub struct RuntimeError {
    pub message: String,
    code: Option<String>,
    pub coordinate: Option<String>,
    pub subgraph_name: Option<String>,
    pub path: String,
    pub extensions: Map<ByteString, Value>,
}

impl Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let subgraph_name = if let Some(subgraph) = &self.subgraph_name {
            format!("{}::", subgraph)
        } else {
            String::new()
        };
        let coordinate = if let Some(coordinate) = &self.coordinate {
            format!("@{}", coordinate)
        } else {
            String::new()
        };
        let extensions = serde_json::to_string_pretty(&self.extensions).unwrap_or_default();
        write!(
            f,
            "Connector Runtime Error ({}): {}\nPATH: {subgraph_name}{}{coordinate}\n{extensions}",
            self.code(),
            self.message,
            self.path,
        )
    }
}

impl RuntimeError {
    pub fn new(message: impl Into<String>, response_key: &ResponseKey) -> Self {
        Self {
            message: message.into(),
            code: None,
            coordinate: None,
            subgraph_name: None,
            path: response_key.path_string(),
            extensions: Default::default(),
        }
    }

    pub fn extension<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<ByteString>,
        V: Into<Value>,
    {
        self.extensions.insert(key.into(), value.into());
        self
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn code(&self) -> &str {
        self.code.as_deref().unwrap_or("CONNECTORS_FETCH")
    }
}

/// An error sending a connector request. This represents a problem with sending the request
/// to the connector, rather than an error returned from the connector itself.
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error("Request limit exceeded")]
    RequestLimitExceeded,

    #[error("Rate limit exceeded")]
    RateLimited,

    #[error("Gateway timeout")]
    GatewayTimeout,

    #[error("Connector error: {0}")]
    TransportFailure(String),
}

impl Error {
    pub fn to_runtime_error(
        &self,
        connector: &Connector,
        response_key: &ResponseKey,
    ) -> RuntimeError {
        RuntimeError {
            message: self.to_string(),
            code: Some(self.code().to_string()),
            coordinate: Some(connector.id.coordinate()),
            subgraph_name: Some(connector.id.subgraph_name.clone()),
            path: response_key.path_string(),
            extensions: Default::default(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::RequestLimitExceeded => "REQUEST_LIMIT_EXCEEDED",
            Self::RateLimited => "REQUEST_RATE_LIMITED",
            Self::GatewayTimeout => "GATEWAY_TIMEOUT",
            Self::TransportFailure(_) => "HTTP_CLIENT_ERROR",
        }
    }
}
