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
    fn from(request: RouterRequest) -> Self {
        crate::RouterRequest {
            http_request: Request::new(crate::Request {
                query: request.query,
                operation_name: request.operation_name,
                variables: request.variables.unwrap_or_default(),
                extensions: request.extensions.unwrap_or_default(),
            })
            .into(),
            context: request.context.unwrap_or_default(),
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

#[derive(Default, Clone, TypedBuilder)]
#[builder(field_defaults(default, setter(strip_option)))]
pub struct ExecutionRequest {
    query_plan: Option<Arc<QueryPlan>>,
    context: Option<Context<CompatRequest>>,
}

impl From<ExecutionRequest> for crate::ExecutionRequest {
    fn from(execution_request: ExecutionRequest) -> Self {
        Self {
            query_plan: execution_request.query_plan.unwrap_or_default(),
            context: execution_request.context.unwrap_or_else(|| {
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
            context: execution_response.context.unwrap_or_else(|| {
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
