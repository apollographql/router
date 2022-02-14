use crate::{Context, Error, Object, Path, QueryPlan};
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
    context: Option<Context<()>>,
}

impl From<RouterRequest> for crate::RouterRequest {
    fn from(rr: RouterRequest) -> Self {
        Self {
            http_request: Request::new(crate::Request {
                query: rr.query,
                operation_name: rr.operation_name,
                variables: rr.variables.unwrap_or_default(),
                extensions: rr.extensions.unwrap_or_default(),
            })
            .into(),
            context: rr.context.unwrap_or_default(),
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
    fn from(rr: RouterResponse) -> Self {
        rr.with_status(StatusCode::OK)
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

#[derive(Default, Clone, TypedBuilder)]
#[builder(field_defaults(default, setter(strip_option)))]
pub struct ExecutionRequest {
    query_plan: Option<Arc<QueryPlan>>,
    context: Option<Context<CompatRequest>>,
}

impl From<ExecutionRequest> for crate::ExecutionRequest {
    fn from(er: ExecutionRequest) -> Self {
        Self {
            query_plan: er.query_plan.unwrap_or_default(),
            context: er.context.unwrap_or_else(|| {
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
    context: Option<Context<CompatRequest>>,
}

impl From<ExecutionResponse> for crate::ExecutionResponse {
    fn from(er: ExecutionResponse) -> Self {
        let mut response_builder = Response::builder().status(er.status);

        for (name, value) in er.headers {
            response_builder = response_builder.header(name, value);
        }
        let response = response_builder
            .body(crate::Response {
                label: er.label,
                data: er.data.unwrap_or_default(),
                path: er.path,
                has_next: er.has_next,
                errors: er.errors,
                extensions: er.extensions.unwrap_or_default(),
            })
            .expect("crate::Response implements Serialize; qed")
            .into();

        Self {
            response,
            context: er.context.unwrap_or_else(|| {
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
