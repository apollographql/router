/// Simple query planner that does no caching.
use super::model::QueryPlan;
use super::{QueryPlanOptions, QueryPlannerError};
pub use harmonizer::plan::PlanningErrors;
use harmonizer::plan::{plan, OperationalContext};
use std::sync::Arc;

/// A query planner that calls out to the nodejs harmonizer query planner.
/// No caching is performed. To cache wrap in a `CachingQueryPlanner`.
#[derive(Debug)]
pub struct HarmonizerQueryPlanner {
    schema: String,
}

impl HarmonizerQueryPlanner {
    /// Create a new harmonizer query planner
    pub fn new(schema: String) -> Self {
        Self { schema }
    }
}

impl super::QueryPlanner for HarmonizerQueryPlanner {
    fn get(
        &self,
        query: String,
        operation: Option<String>,
        options: QueryPlanOptions,
    ) -> Result<QueryPlan, QueryPlannerError> {
        let context = OperationalContext {
            schema: self.schema.clone(),
            query,
            operation: operation.unwrap_or_default(),
        };

        let result = plan(context, options.into())?;
        let parsed = serde_json::from_str::<QueryPlan>(result.as_str())?;
        Ok(parsed)
    }
}

impl From<QueryPlanOptions> for harmonizer::plan::QueryPlanOptions {
    fn from(_: QueryPlanOptions) -> Self {
        harmonizer::plan::QueryPlanOptions::default()
    }
}

impl From<harmonizer::plan::PlanningErrors> for QueryPlannerError {
    fn from(err: PlanningErrors) -> Self {
        QueryPlannerError::PlanningErrors(Arc::new(err))
    }
}

impl From<serde_json::Error> for QueryPlannerError {
    fn from(err: serde_json::Error) -> Self {
        QueryPlannerError::ParseError(Arc::new(err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query_planner::model::FetchNode;
    use crate::query_planner::model::PlanNode::Fetch;
    use crate::query_planner::QueryPlanner;

    #[test]
    fn test_plan() {
        let planner = HarmonizerQueryPlanner::new(include_str!("testdata/schema.graphql").into());
        let result = planner.get(
            include_str!("testdata/query.graphql").into(),
            None,
            QueryPlanOptions::default(),
        );
        assert_eq!(
            QueryPlan {
                node: Some(Fetch(FetchNode {
                    service_name: "accounts".into(),
                    requires: None,
                    variable_usages: vec![],
                    operation: "{me{name{first last}}}".into()
                }))
            },
            result.unwrap()
        );
    }

    #[test]
    fn test_plan_error() {
        let planner = HarmonizerQueryPlanner::new("".to_string());
        let result = planner.get("".into(), None, QueryPlanOptions::default());
        assert_eq!(
            "Query planning had errors: Planning errors: UNKNOWN: Syntax Error: Unexpected <EOF>.",
            result.unwrap_err().to_string()
        );
    }
}
