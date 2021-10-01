//! Calls out to nodejs query planner

use crate::prelude::graphql::*;
use harmonizer::plan;

/// A query planner that calls out to the nodejs harmonizer query planner.
///
/// No caching is performed. To cache, wrap in a [`CachingQueryPlanner`].
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

impl QueryPlanner for HarmonizerQueryPlanner {
    fn get(
        &self,
        query: String,
        operation: Option<String>,
        options: QueryPlanOptions,
    ) -> Result<QueryPlan, QueryPlannerError> {
        let context = plan::OperationalContext {
            schema: self.schema.clone(),
            query,
            operation: operation.unwrap_or_default(),
        };

        let result = plan::plan(context, options.into())?;
        let parsed = serde_json::from_str::<QueryPlan>(result.as_str())?;
        Ok(parsed)
    }
}

impl From<QueryPlanOptions> for plan::QueryPlanOptions {
    fn from(_: QueryPlanOptions) -> Self {
        plan::QueryPlanOptions::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                node: Some(PlanNode::Fetch(FetchNode {
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
