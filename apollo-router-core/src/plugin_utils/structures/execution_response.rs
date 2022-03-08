use super::from_names_and_values;
use crate::{http_compat::Request, Context, Error, Object, Path};
use http::{Response, StatusCode};
use serde_json_bytes::Value;
use std::sync::Arc;
use typed_builder::TypedBuilder;

#[derive(Default, Clone, TypedBuilder)]
#[builder(field_defaults(default, setter(strip_option)))]
pub struct ExecutionResponse {
    label: Option<String>,
    data: Option<Value>,
    path: Option<Path>,
    has_next: Option<bool>,
    #[builder(setter(!strip_option))]
    errors: Vec<Error>,
    #[builder(default, setter(!strip_option, transform = |extensions: Vec<(&str, Value)>| Some(from_names_and_values(extensions))))]
    extensions: Option<Object>,
    #[builder(default = StatusCode::OK, setter(!strip_option))]
    status: StatusCode,
    #[builder(default, setter(!strip_option))]
    headers: Vec<(String, String)>,
    context: Option<Context>,
}

impl From<ExecutionResponse> for crate::ExecutionResponse {
    fn from(execution_response: ExecutionResponse) -> Self {
        let mut response_builder = Response::builder().status(execution_response.status);

        for (name, value) in execution_response.headers {
            response_builder = response_builder.header(name, value);
        }
        let response = response_builder
            .body(crate::Response {
                label: execution_response.label,
                data: execution_response.data.unwrap_or_default(),
                path: execution_response.path,
                has_next: execution_response.has_next,
                errors: execution_response.errors,
                extensions: execution_response.extensions.unwrap_or_default(),
            })
            .expect("crate::Response implements Serialize; qed")
            .into();

        Self {
            response,
            context: execution_response
                .context
                .unwrap_or_else(|| Context::new().with_request(Arc::new(Request::mock()))),
        }
    }
}
