use super::{from_names_and_values, CompatRequest};
use crate::{Context, Error, Object, Path};
use http::{Response, StatusCode};
use serde_json_bytes::Value;
use std::sync::Arc;
use typed_builder::TypedBuilder;

#[derive(Default, Clone, TypedBuilder)]
#[builder(field_defaults(default, setter(strip_option)))]
pub struct SubgraphResponse {
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

impl From<SubgraphResponse> for crate::SubgraphResponse {
    fn from(subgraph_response: SubgraphResponse) -> Self {
        let mut response_builder = Response::builder().status(subgraph_response.status);

        for (name, value) in subgraph_response.headers {
            response_builder = response_builder.header(name, value);
        }
        let response = response_builder
            .body(crate::Response {
                label: subgraph_response.label,
                data: subgraph_response.data.unwrap_or_default(),
                path: subgraph_response.path,
                has_next: subgraph_response.has_next,
                errors: subgraph_response.errors,
                extensions: subgraph_response.extensions.unwrap_or_default(),
            })
            .expect("crate::Response implements Serialize; qed")
            .into();

        Self {
            response,
            context: subgraph_response
                .context
                .unwrap_or_else(|| Context::new().with_request(Arc::new(Default::default()))),
        }
    }
}
