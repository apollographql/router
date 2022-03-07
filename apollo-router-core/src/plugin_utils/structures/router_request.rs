use super::from_names_and_values;
use crate::{http_compat::RequestBuilder, Context, Object};
use http::Method;
use reqwest::Url;
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

        let req = RequestBuilder::new(Method::GET, Url::parse("http://default").unwrap())
            .body(req)
            .unwrap();

        crate::RouterRequest {
            context: request.context.unwrap_or_default().with_request(req),
        }
    }
}
