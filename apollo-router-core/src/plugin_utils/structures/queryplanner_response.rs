use super::{from_names_and_values, CompatRequest};
use crate::{Context, Object, QueryPlan};
use futures::executor::block_on;
use serde_json_bytes::Value;
use std::sync::Arc;
use typed_builder::TypedBuilder;

#[derive(Default, Clone, TypedBuilder)]
#[builder(field_defaults(default, setter(strip_option)))]
pub struct QueryPlannerResponse {
    #[builder(default, setter(!strip_option, transform = |extensions: Vec<(&str, Value)>| Some(from_names_and_values(extensions))))]
    extensions: Option<Object>,
    query_plan: Option<Arc<QueryPlan>>,
    context: Option<Context>,
    request: Option<CompatRequest>,
}

impl From<QueryPlannerResponse> for crate::QueryPlannerResponse {
    fn from(queryplanner_response: QueryPlannerResponse) -> Self {
        let context = queryplanner_response.context.unwrap_or_else(|| {
            let ctx =
                Context::new().with_request(queryplanner_response.request.unwrap_or_default());
            if let Some(extensions) = queryplanner_response.extensions {
                block_on(async { *(ctx.extensions().write().await) = extensions });
            }

            ctx
        });
        Self {
            query_plan: queryplanner_response
                .query_plan
                .unwrap_or_else(|| Arc::new(QueryPlan::default())),
            context,
        }
    }
}
