use super::from_names_and_values;
use crate::{http_compat, Context, Object};
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
}

impl From<RouterRequest> for crate::RouterRequest {
    fn from(request: RouterRequest) -> Self {
        let req = crate::Request {
            query: request.query,
            operation_name: request.operation_name,
            variables: request.variables.unwrap_or_default(),
            extensions: request.extensions.unwrap_or_default(),
        };

        let req = Request::builder().uri("http://default").body(req).unwrap();
        let req_compat = http_compat::Request::try_from(req).unwrap();

        crate::RouterRequest {
            context: request.context.unwrap_or_default().with_request(req_compat),
        }
    }
}
