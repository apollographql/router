//! This is the object model for a QueryPlan.
//! It can be used by an executor to create a response stream.
//!
//! QueryPlans are a set of operations that describe how a federated query is processed.

use crate::prelude::graphql::*;
use serde::Deserialize;

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

    fn query_plan() -> QueryPlan {
        QueryPlan {
            node: Some(PlanNode::Sequence {
                nodes: vec![
                    PlanNode::Fetch(FetchNode {
                        service_name: "product".to_owned(),
                        variable_usages: vec![],
                        requires: None,
                        operation: "{topProducts{__typename ...on Book{__typename isbn}...on Furniture{name}}product(upc:\"1\"){__typename ...on Book{__typename isbn}...on Furniture{name}}}".to_owned(),
                    }),
                    PlanNode::Parallel {
                        nodes: vec![
                            PlanNode::Sequence {
                                nodes: vec![
                                    PlanNode::Flatten(FlattenNode {
                                        path: Path::from("topProducts/@"),
                                        node: Box::new(PlanNode::Fetch(FetchNode {
                                            service_name: "books".to_owned(),
                                            variable_usages: vec!["test_variable".into()],
                                            requires: Some(vec![
                                                Selection::InlineFragment(InlineFragment {
                                                    type_condition: Some("Book".to_owned()),
                                                    selections: vec![
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "__typename".to_owned(),
                                                            selections: None,
                                                        }),
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "isbn".to_owned(),
                                                            selections: None,
                                                        })],
                                                })]),
                                            operation: "query($representations:[_Any!]!){_entities(representations:$representations){...on Book{__typename isbn title year}}}".to_owned(),
                                        })),
                                    }),
                                    PlanNode::Flatten(FlattenNode {
                                        path: Path::from("topProducts/@"),
                                        node: Box::new(PlanNode::Fetch(FetchNode {
                                            service_name: "product".to_owned(),
                                            variable_usages: vec![],
                                            requires: Some(vec![
                                                Selection::InlineFragment(InlineFragment {
                                                    type_condition: Some("Book".to_owned()),
                                                    selections: vec![
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "__typename".to_owned(),
                                                            selections: None,
                                                        }),
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "isbn".to_owned(),
                                                            selections: None,
                                                        }),
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "title".to_owned(),
                                                            selections: None,
                                                        }),
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "year".to_owned(),
                                                            selections: None,
                                                        })],
                                                })]),
                                            operation: "query($representations:[_Any!]!){_entities(representations:$representations){...on Book{name}}}".to_owned(),
                                        })),
                                    })]
                            },
                            PlanNode::Sequence {
                                nodes: vec![
                                    PlanNode::Flatten(FlattenNode {
                                        path: Path::from("product"),
                                        node: Box::new(PlanNode::Fetch(FetchNode {
                                            service_name: "books".to_owned(),
                                            variable_usages: vec![],
                                            requires: Some(vec![
                                                Selection::InlineFragment(InlineFragment {
                                                    type_condition: Some("Book".to_owned()),
                                                    selections: vec![
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "__typename".to_owned(),
                                                            selections: None,
                                                        }),
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "isbn".to_owned(),
                                                            selections: None,
                                                        })],
                                                })]),
                                            operation: "query($representations:[_Any!]!){_entities(representations:$representations){...on Book{__typename isbn title year}}}".to_owned(),
                                        })),
                                    }),
                                    PlanNode::Flatten(FlattenNode {
                                        path: Path::from("product"),
                                        node: Box::new(PlanNode::Fetch(FetchNode {
                                            service_name: "product".to_owned(),
                                            variable_usages: vec![],
                                            requires: Some(vec![
                                                Selection::InlineFragment(InlineFragment {
                                                    type_condition: Some("Book".to_owned()),
                                                    selections: vec![
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "__typename".to_owned(),
                                                            selections: None,
                                                        }),
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "isbn".to_owned(),
                                                            selections: None,
                                                        }),
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "title".to_owned(),
                                                            selections: None,
                                                        }),
                                                        Selection::Field(Field {
                                                            alias: None,
                                                            name: "year".to_owned(),
                                                            selections: None,
                                                        })],
                                                })]),
                                            operation: "query($representations:[_Any!]!){_entities(representations:$representations){...on Book{name}}}".to_owned(),
                                        })),
                                    })]
                            }]
                    }]
            })
        }
    }

    #[test]
    fn query_plan_from_json() {
        assert_eq!(
            serde_json::from_str::<QueryPlan>(test_query_plan!()).unwrap(),
            query_plan()
        );
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
