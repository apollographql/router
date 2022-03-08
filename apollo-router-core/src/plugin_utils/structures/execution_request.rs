use super::CompatRequest;
use crate::{http_compat::Request, Context, QueryPlan};
use std::sync::Arc;
use typed_builder::TypedBuilder;

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
            context: execution_request
                .context
                .unwrap_or_else(|| Context::new().with_request(Arc::new(Request::mock()))),
        }
    }
}
