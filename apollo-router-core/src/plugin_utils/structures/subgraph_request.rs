use super::from_names_and_values;
use crate::{fetch::OperationKind, http_compat, Context, Object};
use http::Request;
use serde_json_bytes::Value;
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
}

impl From<SubgraphRequest> for crate::SubgraphRequest {
    fn from(request: SubgraphRequest) -> Self {
        let gql_req = crate::Request {
            query: request.query,
            operation_name: request.operation_name,
            variables: request.variables.unwrap_or_default(),
            extensions: request.extensions.unwrap_or_default(),
        };
        let req_compat = http_compat::Request::from(Request::new(gql_req));
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
