use std::sync::Arc;

use http::Request;
use serde_json_bytes::{ByteString, Value};

use crate::{Context, Object};

#[derive(Default, Clone)]
pub struct RouterRequestBuilder {
    query: Option<String>,
    operation_name: Option<String>,
    variables: Option<Arc<Object>>,
    extensions: Option<Object>,
    context: Option<Context<()>>,
}

impl RouterRequestBuilder {
    pub fn new() -> Self {
        Default::default()
    }
    pub fn build(&self) -> crate::RouterRequest {
        let this = self.clone();
        crate::RouterRequest {
            http_request: Request::new(crate::Request {
                query: this.query,
                operation_name: this.operation_name,
                variables: this.variables.unwrap_or_default(),
                extensions: this.extensions.unwrap_or_default(),
            }),
            context: this.context.unwrap_or_default(),
        }
    }
    pub fn with_query(self, query: impl AsRef<str>) -> Self {
        Self {
            query: Some(query.as_ref().to_string()),
            ..self
        }
    }
    pub fn with_operation_name(self, operation_name: impl AsRef<str>) -> Self {
        Self {
            operation_name: Some(operation_name.as_ref().to_string()),
            ..self
        }
    }
    pub fn with_variables(self, variables: Arc<Object>) -> Self {
        Self {
            variables: Some(variables),
            ..self
        }
    }
    pub fn with_named_extension(self, name: impl AsRef<str>, value: Value) -> Self {
        let mut extensions = self.extensions.unwrap_or_default();
        extensions.insert(ByteString::from(name.as_ref().to_string()), value);
        Self {
            extensions: Some(extensions),
            ..self
        }
    }
    pub fn with_context(self, context: Context<()>) -> Self {
        Self {
            context: Some(context),
            ..self
        }
    }
}
