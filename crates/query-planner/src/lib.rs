//! Calls out to nodejs query planner
//! Object model for query plan
pub mod caching;
pub mod model;

use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum QueryPlannerError {
    #[error("Query plan was malformed: {parse_error}")]
    ParseError { parse_error: String },

    #[error("Query planning had errors: {planning_errors}")]
    PlanningErrors { planning_errors: String },
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub struct QueryPlanOptions {}

impl QueryPlanOptions {
    pub fn default() -> QueryPlanOptions {
        QueryPlanOptions {}
    }
}

pub trait QueryPlanner: Send + Sync {
    fn get(
        &mut self,
        query: &str,
        operation: &str,
        options: QueryPlanOptions,
    ) -> &Result<model::QueryPlan, QueryPlannerError>;
}
