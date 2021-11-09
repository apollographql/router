//! Calls out to nodejs query planner

use std::{collections::HashMap, sync::Arc};

use crate::prelude::graphql::*;
use apollo_parser::ast;
use async_trait::async_trait;
use router_bridge::plan;

/// A query planner that calls out to the nodejs router-bridge query planner.
///
/// No caching is performed. To cache, wrap in a [`CachingQueryPlanner`].
#[derive(Debug)]
pub struct RouterBridgeQueryPlanner {
    schema: Arc<Schema>,
}

impl RouterBridgeQueryPlanner {
    /// Create a new router-bridge query planner
    pub fn new(schema: Arc<Schema>) -> Self {
        Self { schema }
    }
}

#[async_trait]
impl QueryPlanner for RouterBridgeQueryPlanner {
    async fn get(
        &self,
        query: String,
        operation: Option<String>,
        options: QueryPlanOptions,
    ) -> Result<QueryPlan, QueryPlannerError> {
        let context = plan::OperationalContext {
            schema: self.schema.as_str().to_string(),
            query: query.clone(),
            operation_name: operation.unwrap_or_default(),
        };

        let schema = self.schema.as_str().to_string();

        tokio::task::spawn_blocking(move || {
            let js_query_plan: JsQueryPlan = plan::plan(context, options.into())
                .map_err(|e| QueryPlannerError::PlanningErrors(Arc::new(e)))?;

            let mut query_plan = QueryPlan {
                node: js_query_plan.node,
                operations: Vec::new(),
                fragments: HashMap::new(),
            };

            let schema_parser = apollo_parser::Parser::new(&schema);
            let schema_tree = schema_parser.parse();

            if !schema_tree.errors().is_empty() {
                let errors = schema_tree
                    .errors()
                    .iter()
                    .map(|err| format!("{:?}", err))
                    .collect::<Vec<_>>();
                panic!("Parsing error(s): {}", errors.join(", "));
            }

            let document = schema_tree.document();
            for definition in document.definitions() {
                match definition {
                    ast::Definition::FragmentDefinition(fragment_definition) => {
                        let fragment: Fragment = fragment_definition.into();
                        query_plan
                            .fragments
                            .insert(fragment.fragment_name.clone(), fragment);
                    }
                    _ => {}
                }
            }

            let parser = apollo_parser::Parser::new(&query);
            let tree = parser.parse();

            if !tree.errors().is_empty() {
                let errors = tree
                    .errors()
                    .iter()
                    .map(|err| format!("{:?}", err))
                    .collect::<Vec<_>>();
                panic!("Parsing error(s): {}", errors.join(", "));
            }

            let document = tree.document();

            for definition in document.definitions() {
                match definition {
                    // Spec: https://spec.graphql.org/draft/#sec-Language.Operations
                    ast::Definition::OperationDefinition(operation) => {
                        query_plan.operations.push(operation.into());
                    }
                    ast::Definition::FragmentDefinition(fragment_definition) => {
                        let fragment: Fragment = fragment_definition.into();
                        query_plan
                            .fragments
                            .insert(fragment.fragment_name.clone(), fragment);
                    }
                    _ => {}
                }
            }

            Ok(query_plan)
        })
        .await?
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
    use test_env_log::test;

    #[test(tokio::test)]
    async fn test_plan() {
        let planner = RouterBridgeQueryPlanner::new(Arc::new(
            include_str!("testdata/schema.graphql").parse().unwrap(),
        ));
        let result = planner
            .get(
                include_str!("testdata/query.graphql").into(),
                None,
                QueryPlanOptions::default(),
            )
            .await;
        assert_eq!(
            Some(PlanNode::Fetch(FetchNode {
                service_name: "accounts".into(),
                requires: None,
                variable_usages: vec![],
                operation: "{me{name{first last}}}".into()
            })),
            result.unwrap().node
        );
    }

    #[test(tokio::test)]
    async fn test_plan_error() {
        let planner = RouterBridgeQueryPlanner::new(Arc::new("".parse().unwrap()));
        let result = planner
            .get("".into(), None, QueryPlanOptions::default())
            .await;

        assert_eq!(
            "Query planning had errors: Planning errors: UNKNOWN: Syntax Error: Unexpected <EOF>.",
            result.unwrap_err().to_string()
        );
    }
}
