//! Calls out to nodejs query planner
//! Object model for query plan

#[cfg(test)]
use mockall::{automock, predicate::*};
use thiserror::Error;

/// A caching query planner decorator
pub mod caching;

/// The query plan model
pub mod model;

/// A query planner that calls out to the nodejs harmonizer
pub mod harmonizer;

/// Error types for QueryPlanner
#[derive(Error, Debug, Clone)]
pub enum QueryPlannerError {
    /// The json returned from harmonizer was not well formed.
    #[error("Query plan was malformed: {parse_errors}")]
    ParseError {
        /// The error messages.
        parse_errors: String,
    },

    /// The query planner generated errors.
    #[error("Query planning had errors: {planning_errors}")]
    PlanningErrors {
        /// The error messages.
        planning_errors: String,
    },
}

/// Query planning options.
#[derive(Clone, Eq, Hash, PartialEq, Debug)]
pub struct QueryPlanOptions {}

/// Query planning options.
impl QueryPlanOptions {
    /// Default query planning options.
    pub fn default() -> QueryPlanOptions {
        QueryPlanOptions {}
    }
}

/// QueryPlanner can be used to plan queries.
/// Implementations may cache query plans.
#[cfg_attr(test, automock)]
pub trait QueryPlanner: Send {
    /// Returns a query plan given the query, operation and options.
    /// Implementations may cache query plans.
    #[must_use = "query plan result must be used"]
    fn get(
        &mut self,
        query: String,
        operation: Option<String>,
        options: QueryPlanOptions,
    ) -> Result<model::QueryPlan, QueryPlannerError>;
}
