use std::sync::Arc;

use crate::{Context, Error, Object, Path};
use http::StatusCode;
use http::{Request, Response};
use serde_json_bytes::{ByteString, Value};

type CompatRequest = Arc<crate::http_compat::Request<crate::Request>>;

#[derive(Default, Clone)]
pub struct RouterResponseBuilder {
    label: Option<String>,
    data: Option<Value>,
    path: Option<Path>,
    has_next: Option<bool>,
    errors: Vec<Error>,
    extensions: Option<Object>,
    context: Option<Context<CompatRequest>>,
}

impl RouterResponseBuilder {
    pub fn new() -> Self {
        Default::default()
    }
    pub fn build(&self) -> crate::RouterResponse {
        self.with_status(StatusCode::OK)
    }
    pub fn with_status(&self, status: StatusCode) -> crate::RouterResponse {
        let this = self.clone();
        crate::RouterResponse {
            response: Response::builder()
                .status(status)
                .body(
                    crate::Response {
                        label: this.label,
                        data: this.data.unwrap_or_default(),
                        path: this.path,
                        has_next: this.has_next,
                        errors: this.errors,
                        extensions: this.extensions.unwrap_or_default(),
                    }
                    .into(),
                )
                .expect("crate::Response implements Serialize; qed")
                .into(),
            context: this.context.unwrap_or_else(|| {
                Context::new().with_request(Arc::new(
                    Request::new(crate::Request {
                        query: Default::default(),
                        operation_name: Default::default(),
                        variables: Default::default(),
                        extensions: Default::default(),
                    })
                    .into(),
                ))
            }),
        }
    }
    pub fn with_label(self, label: impl AsRef<str>) -> Self {
        Self {
            label: Some(label.as_ref().to_string()),
            ..self
        }
    }
    pub fn with_data(self, data: Value) -> Self {
        Self {
            data: Some(data),
            ..self
        }
    }
    pub fn with_path(self, path: Path) -> Self {
        Self {
            path: Some(path),
            ..self
        }
    }
    pub fn with_has_next(self, has_next: bool) -> Self {
        Self {
            has_next: Some(has_next),
            ..self
        }
    }
    pub fn push_error(mut self, error: Error) -> Self {
        self.errors.push(error);

        Self { ..self }
    }
    pub fn with_named_extension(self, name: impl AsRef<str>, value: Value) -> Self {
        let mut extensions = self.extensions.unwrap_or_default();
        extensions.insert(ByteString::from(name.as_ref().to_string()), value);
        Self {
            extensions: Some(extensions),
            ..self
        }
    }
    pub fn with_context(self, context: Context<CompatRequest>) -> Self {
        Self {
            context: Some(context),
            ..self
        }
    }
}
