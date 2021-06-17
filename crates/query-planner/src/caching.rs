/// Caching query planner caches query plans based on the request, schema and options.
/// It is not thread safe currently.
use crate::model::QueryPlan;
use crate::{QueryPlanOptions, QueryPlannerError};
use harmonizer::plan::{plan, OperationalContext, PlanningErrors};
use serde_json::de::from_str;
use serde_json::Error;
use std::collections::HashMap;

pub struct CachingQueryPlanner {
    schema: String,
    cached:
        HashMap<(String, String, crate::QueryPlanOptions), Result<QueryPlan, QueryPlannerError>>,
}

impl CachingQueryPlanner {
    pub fn new(schema: &str) -> CachingQueryPlanner {
        CachingQueryPlanner {
            schema: schema.into(),
            cached: HashMap::new(),
        }
    }
}

impl crate::QueryPlanner for CachingQueryPlanner {
    fn get(
        &mut self,
        query: &str,
        operation: &str,
        options: QueryPlanOptions,
    ) -> Result<&QueryPlan, &QueryPlannerError> {
        let schema = &self.schema;
        self.cached
            .entry((query.into(), operation.into(), options))
            .or_insert_with(|| {
                let context = OperationalContext {
                    schema: schema.to_string(),
                    query: query.to_string(),
                    operation: operation.to_string(),
                };

                let result = plan(context, harmonizer::plan::QueryPlanOptions::default())?;
                let parsed = from_str::<QueryPlan>(result.as_str())?;
                Ok(parsed)
            })
            .as_ref()
    }
}

impl From<harmonizer::plan::PlanningErrors> for QueryPlannerError {
    fn from(e: PlanningErrors) -> Self {
        QueryPlannerError::PlanningErrors {
            planning_errors: e.to_string(),
        }
    }
}

impl From<serde_json::Error> for QueryPlannerError {
    fn from(e: Error) -> Self {
        QueryPlannerError::ParseError {
            parse_error: e.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FetchNode;
    use crate::model::PlanNode::Fetch;
    use crate::QueryPlanner;

    #[test]
    fn test_plan() {
        let mut planner = CachingQueryPlanner::new(include_str!("testdata/schema.graphql"));
        let result = planner.get(
            include_str!("testdata/query.graphql"),
            "",
            QueryPlanOptions::default(),
        );
        assert_eq!(
            &QueryPlan {
                node: Some(Fetch(FetchNode {
                    service_name: "accounts".to_owned(),
                    requires: None,
                    variable_usages: vec![],
                    operation: "{me{name{first last}}}".to_owned()
                }))
            },
            result.unwrap()
        );
    }

    #[test]
    fn test_plan_error() {
        let mut planner = CachingQueryPlanner::new("");
        let result = planner.get("", "", QueryPlanOptions::default());
        assert_eq!(
            "Query planning had errors: Planning errors: UNKNOWN: Syntax Error: Unexpected <EOF>.",
            result.unwrap_err().to_string()
        );
    }
}
