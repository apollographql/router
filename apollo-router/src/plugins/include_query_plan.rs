use futures::future::ready;
use futures::stream::once;
use futures::StreamExt;
use serde_json_bytes::json;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceExt as TowerServiceExt;

use crate::layers::ServiceExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::QueryPlannerContent;
use crate::services::QueryPlannerRequest;
use crate::services::QueryPlannerResponse;
use crate::services::RouterRequest;
use crate::services::RouterResponse;

const QUERY_PLAN_CONTEXT_KEY: &str = "apollo::include_query_plan.plan";

#[derive(Debug, Clone)]
struct IncludeQueryPlan {
    enabled: bool,
}

#[async_trait::async_trait]
impl Plugin for IncludeQueryPlan {
    type Config = bool;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(IncludeQueryPlan {
            enabled: init.config,
        })
    }

    fn query_planning_service(
        &self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        let is_enabled = self.enabled;
        service
            .map_response(move |res| {
                if is_enabled {
                    if let QueryPlannerContent::Plan { plan, .. } = &res.content {
                        res.context
                            .insert(QUERY_PLAN_CONTEXT_KEY, plan.root.clone())
                            .unwrap();
                    }
                }

                res
            })
            .boxed()
    }

    fn router_service(
        &self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        let is_enabled = self.enabled;

        service
            .map_future_with_context(|req: &RouterRequest| {
                req.originating_request.body().query.clone()
            }, move |query: Option<String>, f| async move {
                let mut res: Result<RouterResponse, BoxError>  = f.await;
                res = match res {
                    Ok(mut res) => {
                        if is_enabled {
                            let (parts, stream) = http::Response::from(res.response).into_parts();
                            let (mut first, rest) = stream.into_future().await;

                            if let Some(first) = &mut first {
                                if let Some(plan) =
                                    res.context.get_json_value(QUERY_PLAN_CONTEXT_KEY)
                                {
                                    first
                                        .extensions
                                        .insert("apolloQueryPlan", json!({ "object": { "kind": "QueryPlan", "node": plan, "text": query } }));
                                }
                            }

                            res.response = http::Response::from_parts(
                                parts,
                                once(ready(first.unwrap_or_default())).chain(rest).boxed(),
                            )
                            .into();
                        }
                        Ok(res)
                    }
                    Err(err) => Err(err),
                };

                res
            })
            .boxed()
    }
}

register_plugin!("apollo", "include_query_plan", IncludeQueryPlan);

#[cfg(test)]
mod tests {
    // TODO
}
