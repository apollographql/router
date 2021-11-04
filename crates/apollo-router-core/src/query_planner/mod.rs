//! Calls out to nodejs query planner
//! Object model for query plan

mod caching;
mod harmonizer;
mod model;

use displaydoc::Display;
#[cfg(test)]
use mockall::{automock, predicate::*};
use std::sync::Arc;
use thiserror::Error;

pub use self::harmonizer::*;
pub use caching::*;
pub use model::*;

/// Error types for QueryPlanner
#[derive(Error, Debug, Display, Clone)]
pub enum QueryPlannerError {
    /// Query plan was malformed: {0}
    ParseError(Arc<serde_json::Error>),

    /// Query planning had errors: {0}
    PlanningErrors(Arc<harmonizer::PlanningErrors>),
}

/// Query planning options.
#[derive(Clone, Eq, Hash, PartialEq, Debug)]
pub struct QueryPlanOptions {}

/// Query planning options.
impl Default for QueryPlanOptions {
    /// Default query planning options.
    fn default() -> QueryPlanOptions {
        QueryPlanOptions {}
    }
}

/// QueryPlanner can be used to plan queries.
/// Implementations may cache query plans.
#[cfg_attr(test, automock)]
pub trait QueryPlanner: Send + Sync {
    /// Returns a query plan given the query, operation and options.
    /// Implementations may cache query plans.
    #[must_use = "query plan result must be used"]
    fn get(
        &self,
        query: String,
        operation: Option<String>,
        options: QueryPlanOptions,
    ) -> Result<model::QueryPlan, QueryPlannerError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use static_assertions::*;

    assert_obj_safe!(QueryPlanner);
}
