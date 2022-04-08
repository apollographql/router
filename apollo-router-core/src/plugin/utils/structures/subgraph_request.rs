use super::from_names_and_values;
use crate::{fetch::OperationKind, http_compat, Context, Object, Request};
use http::header::{HeaderName, HeaderValue};
use http::{Method, Uri};
use serde_json_bytes::Value;
use std::str::FromStr;
use std::sync::Arc;
use typed_builder::TypedBuilder;

#[derive(Default, Clone, TypedBuilder)]
#[builder(field_defaults(default, setter(strip_option)))]
pub struct SubgraphRequest {
    query: Option<String>,
    operation_name: Option<String>,
    operation_kind: Option<OperationKind>,
    variables: Option<Arc<Object>>,
    #[builder(default, setter(!strip_option, transform = |extensions: Vec<(&str, Value)>| Some(from_names_and_values(extensions))))]
    extensions: Option<Object>,
    context: Option<Context<()>>,
    headers: Option<Vec<(String, String)>>,
}

impl From<SubgraphRequest> for crate::SubgraphRequest {
    fn from(request: SubgraphRequest) -> Self {
        let gql_req = crate::Request {
            query: request.query,
            operation_name: request.operation_name,
            variables: request.variables.unwrap_or_default(),
            extensions: request.extensions.unwrap_or_default(),
        };
        let mut req_compat: http_compat::Request<Request> =
            http_compat::RequestBuilder::new(Method::GET, Uri::from_str("http://default").unwrap())
                .body(gql_req)
                .expect("won't fail because our url is valid; qed");

        for (key, value) in request.headers.unwrap_or_default() {
            req_compat.headers_mut().insert(
                HeaderName::from_str(key.as_str()).expect("name must be valid"),
                HeaderValue::from_str(value.as_str()).expect("value must be valid"),
            );
        }

        crate::SubgraphRequest {
            context: request
                .context
                .unwrap_or_default()
                .with_request(Arc::new(req_compat.clone())),
            http_request: req_compat,
            operation_kind: request.operation_kind.unwrap_or(OperationKind::Query),
        }
    }
}
