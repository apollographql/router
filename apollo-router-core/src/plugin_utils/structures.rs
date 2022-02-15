use crate::{http_compat, Context, Error, Object, Path};
use http::{Request, Response, StatusCode};
use serde_json_bytes::{ByteString, Value};
use std::sync::Arc;
use typed_builder::TypedBuilder;

#[derive(Default, Clone, TypedBuilder)]
#[builder(field_defaults(default, setter(strip_option)))]
pub struct RouterRequest {
    #[builder(setter(!strip_option))]
    query: String,
    operation_name: Option<String>,
    variables: Option<Arc<Object>>,
    #[builder(default, setter(!strip_option, transform = |extensions: Vec<(&str, Value)>| Some(from_names_and_values(extensions))))]
    extensions: Option<Object>,
    context: Option<Context<http_compat::Request<crate::Request>>>,
}

impl From<RouterRequest> for crate::RouterRequest {
    fn from(request: RouterRequest) -> Self {
        let req = Request::new(crate::Request {
            query: request.query,
            operation_name: request.operation_name,
            variables: request.variables.unwrap_or_default(),
            extensions: request.extensions.unwrap_or_default(),
        });
        crate::RouterRequest {
            context: request
                .context
                .unwrap_or_else(|| Context::new().with_request(req.into())),
        }
    }
}

type CompatRequest = Arc<crate::http_compat::Request<crate::Request>>;

#[derive(Default, Clone, TypedBuilder)]
#[builder(field_defaults(default, setter(strip_option)))]
pub struct RouterResponse {
    label: Option<String>,
    data: Option<Value>,
    path: Option<Path>,
    has_next: Option<bool>,
    #[builder(setter(!strip_option))]
    errors: Vec<Error>,
    #[builder(default, setter(!strip_option, transform = |extensions: Vec<(&str, Value)>| Some(from_names_and_values(extensions))))]
    extensions: Option<Object>,
    context: Option<Context<CompatRequest>>,
}

impl From<RouterResponse> for crate::RouterResponse {
    fn from(response: RouterResponse) -> Self {
        response.with_status(StatusCode::OK)
    }
}

impl RouterResponse {
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
}

fn from_names_and_values(extensions: Vec<(&str, Value)>) -> Object {
    extensions
        .into_iter()
        .map(|(name, value)| (ByteString::from(name.to_string()), value))
        .collect()
}
