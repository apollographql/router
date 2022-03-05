use super::from_names_and_values;
use crate::{Context, Object};
use http::Request;
use serde_json_bytes::Value;
use std::sync::Arc;
use typed_builder::TypedBuilder;

#[derive(Default, Clone, TypedBuilder)]
#[builder(field_defaults(default, setter(strip_option)))]
pub struct RouterRequest {
    query: Option<String>,
    operation_name: Option<String>,
    variables: Option<Arc<Object>>,
    #[builder(default, setter(!strip_option, transform = |extensions: Vec<(&str, Value)>| Some(from_names_and_values(extensions))))]
    extensions: Option<Object>,
    context: Option<Context<()>>,
    headers: Option<Vec<(String, String)>>,
}

impl From<RouterRequest> for crate::RouterRequest {
    fn from(request: RouterRequest) -> Self {
        let mut req = Request::builder();
        for (key, value) in request.headers.unwrap_or_default() {
            req = req.header(key, value);
        }
        let req = req
            .body(crate::Request {
                query: request.query,
                operation_name: request.operation_name,
                variables: request.variables.unwrap_or_default(),
                extensions: request.extensions.unwrap_or_default(),
            })
            .expect("body is always valid; qed");
        crate::RouterRequest {
            context: request.context.unwrap_or_default().with_request(req.into()),
        }
    }
}
