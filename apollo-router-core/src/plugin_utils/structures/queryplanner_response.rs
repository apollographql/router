use super::CompatRequest;
use crate::{http_compat::RequestBuilder, Context, QueryPlan};
use http::Method;
use reqwest::Url;
use std::sync::Arc;
use typed_builder::TypedBuilder;

#[derive(Default, Clone, TypedBuilder)]
#[builder(field_defaults(default, setter(strip_option)))]
pub struct QueryPlannerResponse {
    query_plan: Option<Arc<QueryPlan>>,
    context: Option<Context>,
    request: Option<CompatRequest>,
}

impl From<QueryPlannerResponse> for crate::QueryPlannerResponse {
    fn from(queryplanner_response: QueryPlannerResponse) -> Self {
        let context = queryplanner_response.context.unwrap_or_else(|| {
            Context::new().with_request(queryplanner_response.request.unwrap_or_else(|| {
                Arc::new(
                    RequestBuilder::new(Method::GET, Url::parse("http://default").unwrap())
                        .body(crate::Request::default())
                        .unwrap(),
                )
            }))
        });
        Self {
            query_plan: queryplanner_response
                .query_plan
                .unwrap_or_else(|| Arc::new(QueryPlan::default())),
            context,
        }
    }
}
