use apollo_federation::query_plan as next;

use crate::query_planner::bridge_query_planner as bridge;

impl From<&'_ next::QueryPlan> for bridge::QueryPlan {
    fn from(_plan: &'_ next::QueryPlan) -> Self {
        todo!()
    }
}
