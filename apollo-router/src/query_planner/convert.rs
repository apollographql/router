use apollo_federation::query_plan as next;

use crate::query_planner::bridge_query_planner as bridge;

// TODO: port in federation-next:
// https://github.com/apollographql/federation/blob/%40apollo/query-planner%402.7.1/query-planner-js/src/prettyFormatQueryPlan.ts
pub(crate) fn formatted(_plan: &next::QueryPlan) -> String {
    todo!()
}

// TODO: replace with https://github.com/apollographql/router/pull/4796
pub(crate) fn usage_reporting(_plan: &next::QueryPlan) -> router_bridge::planner::UsageReporting {
    todo!()
}

impl From<next::QueryPlan> for bridge::QueryPlan {
    fn from(_plan: next::QueryPlan) -> Self {
        todo!()
    }
}
