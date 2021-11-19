//! This is the object model for a QueryPlan.
//! It can be used by an executor to create a response stream.
//!
//! QueryPlans are a set of operations that describe how a federated query is processed.

use crate::prelude::graphql::*;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::Arc;

/// The root query plan container.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "kind")]
pub struct QueryPlan {
    /// The hierarchical nodes that make up the query plan
    node: Option<PlanNode>,
}

impl QueryPlan {
    /// Returns a reference to the plan.
    pub fn node(&self) -> Option<&PlanNode> {
        self.node.as_ref()
    }
}

/// Query plans are composed of a set of nodes.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "PascalCase", tag = "kind")]
pub enum PlanNode {
    /// These nodes must be executed in order.
    Sequence {
        /// The plan nodes that make up the sequence execution.
        nodes: Vec<PlanNode>,
    },

    /// These nodes may be executed in parallel.
    Parallel {
        /// The plan nodes that make up the parallel execution.
        nodes: Vec<PlanNode>,
    },

    /// Fetch some data from a subgraph.
    Fetch(FetchNode),

    /// Merge the current resultset with the response.
    Flatten(FlattenNode),
}

impl PlanNode {
    /// Retrieves all the variables used across all plan nodes.
    ///
    /// Note that duplicates are not filtered.
    pub fn variable_usage<'a>(&'a self) -> Box<dyn Iterator<Item = &'a str> + 'a> {
        match self {
            Self::Sequence { nodes } | Self::Parallel { nodes } => {
                Box::new(nodes.iter().flat_map(|x| x.variable_usage()))
            }
            Self::Fetch(fetch) => Box::new(fetch.variable_usages.iter().map(|x| x.as_str())),
            Self::Flatten(flatten) => Box::new(flatten.node.variable_usage()),
        }
    }

    /// Retrieves all the services used across all plan nodes.
    ///
    /// Note that duplicates are not filtered.
    pub fn service_usage<'a>(&'a self) -> Box<dyn Iterator<Item = &'a str> + 'a> {
        match self {
            Self::Sequence { nodes } | Self::Parallel { nodes } => {
                Box::new(nodes.iter().flat_map(|x| x.service_usage()))
            }
            Self::Fetch(fetch) => Box::new(vec![fetch.service_name.as_str()].into_iter()),
            Self::Flatten(flatten) => Box::new(flatten.node.service_usage()),
        }
    }

    /// Recursively validate a query plan node making sure that all services are known before we go
    /// for execution.
    ///
    /// This simplifies processing later as we can always guarantee that services are configured for
    /// the plan.
    ///
    /// # Arguments
    ///
    ///  *   `plan`: The root query plan node to validate.
    pub fn validate_services_against_plan(
        &self,
        service_registry: Arc<dyn ServiceRegistry>,
    ) -> Vec<FetchError> {
        self.service_usage()
            .collect::<HashSet<_>>()
            .into_iter()
            .filter(|service| !service_registry.has(service))
            .map(|service| FetchError::ValidationUnknownServiceError {
                service: service.to_string(),
            })
            .collect::<Vec<_>>()
    }

    /// Recursively validate a query plan node making sure that all variable usages are known before we
    /// go for execution.
    ///
    /// This simplifies processing later as we can always guarantee that the variable usages are
    /// available for the plan.
    ///
    /// # Arguments
    ///
    ///  *   `plan`: The root query plan node to validate.
    pub fn validate_request_variables_against_plan(&self, request: &Request) -> Vec<FetchError> {
        let required = self.variable_usage().collect::<HashSet<_>>();
        let provided = request
            .variables
            .as_ref()
            .map(|v| v.keys().map(|x| x.as_str()).collect::<HashSet<_>>())
            .unwrap_or_default();
        required
            .difference(&provided)
            .map(|x| FetchError::ValidationMissingVariable {
                name: x.to_string(),
            })
            .collect::<Vec<_>>()
    }

    /// TODO
    #[tracing::instrument(name = "validation")]
    pub fn validate_request(
        &self,
        request: &Request,
        service_registry: Arc<dyn ServiceRegistry>,
    ) -> Result<(), Response> {
        let mut early_errors = Vec::new();
        for err in self.validate_services_against_plan(Arc::clone(&service_registry)) {
            early_errors.push(err.to_graphql_error(None));
        }

        for err in self.validate_request_variables_against_plan(&request) {
            early_errors.push(err.to_graphql_error(None));
        }

        if !early_errors.is_empty() {
            Err(Response::builder().errors(early_errors).build())
        } else {
            Ok(())
        }
    }
}

/// A fetch node.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchNode {
    /// The name of the service or subgraph that the fetch is querying.
    pub service_name: String,

    /// The data that is required for the subgraph fetch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<Vec<Selection>>,

    /// The variables that are used for the subgraph fetch.
    pub variable_usages: Vec<String>,

    /// The GraphQL subquery that is used for the fetch.
    pub operation: String,
}

/// A flatten node.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlattenNode {
    /// The path when result should be merged.
    pub path: Path,

    /// The child execution plan.
    pub node: Box<PlanNode>,
}

/// A selection that is part of a fetch.
/// Selections are used to propagate data to subgraph fetches.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "PascalCase", tag = "kind")]
pub enum Selection {
    /// A field selection.
    Field(Field),

    /// An inline fragment selection.
    InlineFragment(InlineFragment),
}

/// The field that is used
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    /// An optional alias for the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,

    /// The name of the field.
    pub name: String,

    /// The selections for the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selections: Option<Vec<Selection>>,
}

/// An inline fragment.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineFragment {
    /// The required fragment type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_condition: Option<String>,

    /// The selections from the fragment.
    pub selections: Vec<Selection>,
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! test_query_plan {
        () => {
            include_str!("testdata/query_plan.json")
        };
    }

    #[test]
    fn query_plan_from_json() {
        let query_plan: QueryPlan = serde_json::from_str(test_query_plan!()).unwrap();
        insta::assert_debug_snapshot!(query_plan);
    }

    #[test]
    fn service_usage() {
        assert_eq!(
            serde_json::from_str::<QueryPlan>(test_query_plan!())
                .unwrap()
                .node
                .unwrap()
                .service_usage()
                .collect::<Vec<_>>(),
            vec!["product", "books", "product", "books", "product"]
        );
    }

    #[test]
    fn variable_usage() {
        assert_eq!(
            serde_json::from_str::<QueryPlan>(test_query_plan!())
                .unwrap()
                .node
                .unwrap()
                .variable_usage()
                .collect::<Vec<_>>(),
            vec!["test_variable"]
        );
    }
}
