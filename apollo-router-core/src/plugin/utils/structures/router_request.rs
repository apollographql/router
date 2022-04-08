use super::from_names_and_values;
use crate::{
    http_compat::{self, RequestBuilder},
    Context, Object,
};
use http::{Method, Uri};
use serde_json_bytes::Value;
use std::{str::FromStr, sync::Arc};
use typed_builder::TypedBuilder;

#[derive(Default, Clone, TypedBuilder)]
#[builder(field_defaults(default, setter(strip_option)))]
pub struct RouterRequest {
    query: Option<String>,
    operation_name: Option<String>,
    variables: Option<Arc<Object>>,
    #[builder(default, setter(!strip_option, transform = |extensions: Vec<(&str, Value)>| Some(from_names_and_values(extensions))))]
    extensions: Option<Object>,
    context: Option<Context<http_compat::Request<crate::Request>>>,
    headers: Option<Vec<(String, String)>>,
}

impl From<RouterRequest> for crate::RouterRequest {
    fn from(request: RouterRequest) -> Self {
        let gql_request = crate::Request {
            query: request.query,
            operation_name: request.operation_name,
            variables: request.variables.unwrap_or_default(),
            extensions: request.extensions.unwrap_or_default(),
        };

        let mut req = RequestBuilder::new(Method::GET, Uri::from_str("http://default").unwrap());

        for (key, value) in request.headers.unwrap_or_default() {
            req = req.header(key, value);
        }
        let req = req.body(gql_request).expect("body is always valid; qed");

        crate::RouterRequest {
            context: request
                .context
                .unwrap_or_else(|| Context::new().with_request(req)),
        }
    }
}
