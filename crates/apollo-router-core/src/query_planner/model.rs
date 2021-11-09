//! This is the object model for a QueryPlan.
//! It can be used by an executor to create a response stream.
//!
//! QueryPlans are a set of operations that describe how a federated query is processed.

use std::collections::HashMap;

use crate::prelude::graphql::*;
use apollo_parser::ast;
use serde::{Deserialize, Serialize};

/// The root query plan container.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub struct JsQueryPlan {
    /// The hierarchical nodes that make up the query plan
    pub node: Option<PlanNode>,
}

/*FIXME: I get a panic when calling JS if I add the fields to QueryPlan:
thread 'tokio-runtime-worker' panicked at 'unable to invoke var _a;
Object.defineProperty(exports, "__esModule", { value: true });
const planResult = bridge.plan(schemaString, queryString, operationName);
if (((_a = planResult.errors) === null || _a === void 0 ? void 0 : _a.length) > 0) {
    done({ Err: planResult.errors });
}
else {
    done({ Ok: planResult.data });
}
//# sourceMappingURL=do_plan.js.map in JavaScript runtime
 error:
 Error: Error parsing args: serde_v8 error: ExpectedArray
    at unwrapOpResult (deno:core/core.js:99:13)
    at Object.opSync (deno:core/core.js:113:12)
    at done (<init>:7:15)
    at do_plan:8:5', /home/geal/.cargo/git/checkouts/federation-320f8bad94ab52a0/1ffecef/router-bridge/src/js.rs:105:13
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

*/
/// The root query plan container.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub struct QueryPlan {
    /// The hierarchical nodes that make up the query plan
    pub node: Option<PlanNode>,
    // list of operations to apply on the final response
    #[serde(default)]
    pub operations: Vec<Operation>,
    // list of fragments to apply on the final response
    #[serde(default)]
    pub fragments: HashMap<String, Fragment>,
}

/// Query plans are composed of a set of nodes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlattenNode {
    /// The path when result should be merged.
    pub path: Path,

    /// The child execution plan.
    pub node: Box<PlanNode>,
}

/// A selection that is part of a fetch.
/// Selections are used to propagate data to subgraph fetches.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", tag = "kind")]
pub enum Selection {
    /// A field selection.
    Field(Field),

    /// An inline fragment selection.
    InlineFragment(InlineFragment),

    /// An inline fragment selection.
    FragmentSpread(FragmentSpread),
}

impl From<ast::Selection> for Selection {
    fn from(selection: ast::Selection) -> Selection {
        match selection {
            ast::Selection::Field(field) => Selection::Field(field.into()),
            ast::Selection::InlineFragment(inline_fragment) => {
                Selection::InlineFragment(inline_fragment.into())
            }
            ast::Selection::FragmentSpread(fragment_spread) => {
                Selection::FragmentSpread(fragment_spread.into())
            }
        }
    }
}

/// The field that is used
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

impl From<ast::Field> for Field {
    fn from(field: ast::Field) -> Field {
        Field {
            alias: field
                .alias()
                .as_ref()
                .and_then(|alias| alias.name().as_ref().map(|name| name.text().to_string())),
            name: field
                .name()
                .as_ref()
                .map(|name| name.text().to_string())
                .expect("a field is always named"),
            selections: field
                .selection_set()
                .as_ref()
                .map(|set| set.selections().map(|selection| selection.into()).collect()),
        }
    }
}

/// An inline fragment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineFragment {
    /// The required fragment type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_condition: Option<String>,

    /// The selections from the fragment.
    pub selections: Vec<Selection>,
}

impl From<ast::InlineFragment> for InlineFragment {
    fn from(fragment: ast::InlineFragment) -> InlineFragment {
        InlineFragment {
            type_condition: fragment.type_condition().as_ref().and_then(|ty| {
                ty.named_type()
                    .as_ref()
                    .and_then(|ty| ty.name().as_ref().map(|name| name.text().to_string()))
            }),
            selections: fragment
                .selection_set()
                .as_ref()
                .expect("a fragment always has selections")
                .selections()
                .map(|selection| selection.into())
                .collect(),
        }
    }
}

/// A fragment
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fragment {
    /// name of the fragment
    pub fragment_name: String,

    /// The selections from the fragment.
    pub selections: Vec<Selection>,
}

impl From<ast::FragmentDefinition> for Fragment {
    fn from(fragment: ast::FragmentDefinition) -> Fragment {
        Fragment {
            fragment_name: fragment
                .fragment_name()
                .as_ref()
                .and_then(|fragment_name| fragment_name.name())
                .map(|name| name.text().to_string())
                .expect("the fragment name is always present"),
            selections: fragment
                .selection_set()
                .as_ref()
                .expect("a fragment always has selections")
                .selections()
                .map(|selection| selection.into())
                .collect(),
        }
    }
}

/// A fragment spread
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FragmentSpread {
    /// name of the fragment
    pub fragment_name: String,
    // ignoring the directives for now, we don't execute them
}

impl From<ast::FragmentSpread> for FragmentSpread {
    fn from(fragment: ast::FragmentSpread) -> FragmentSpread {
        FragmentSpread {
            fragment_name: fragment
                .fragment_name()
                .as_ref()
                .and_then(|fragment_name| fragment_name.name())
                .map(|name| name.text().to_string())
                .expect("the fragment name is always present"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    pub name: Option<String>,
    pub selection_set: SelectionSet,
}

impl From<ast::OperationDefinition> for Operation {
    fn from(op: ast::OperationDefinition) -> Operation {
        Operation {
            name: op.name().as_ref().map(|name| name.text().to_string()),
            selection_set: op
                .selection_set()
                .expect("the node SelectionSet is not optional in the spec")
                .into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SelectionSet {
    pub selections: Vec<Selection>,
}

impl From<ast::SelectionSet> for SelectionSet {
    fn from(set: ast::SelectionSet) -> SelectionSet {
        SelectionSet {
            selections: set.selections().map(|selection| selection.into()).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const TYPENAME_FIELD_NAME: &str = "__typename";

    fn qp_json_string() -> String {
        include_str!("testdata/query_plan.json").to_owned()
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
                                                            name: TYPENAME_FIELD_NAME.to_owned(),
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
                                                            name: TYPENAME_FIELD_NAME.to_owned(),
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
                                                            name: TYPENAME_FIELD_NAME.to_owned(),
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
                                                            name: TYPENAME_FIELD_NAME.to_owned(),
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
            }),
            operations: Vec::new(),
            fragments: Vec::new(),
        }
    }

    #[test]
    fn query_plan_from_json() {
        assert_eq!(
            serde_json::from_str::<QueryPlan>(qp_json_string().as_str()).unwrap(),
            query_plan()
        );
    }

    #[test]
    fn service_usage() {
        assert_eq!(
            serde_json::from_str::<QueryPlan>(qp_json_string().as_str())
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
            serde_json::from_str::<QueryPlan>(qp_json_string().as_str())
                .unwrap()
                .node
                .unwrap()
                .variable_usage()
                .collect::<Vec<_>>(),
            vec!["test_variable"]
        );
    }

    #[test]
    fn query_plan_into_json() {
        assert_eq!(
            serde_json::to_value(query_plan()).unwrap(),
            serde_json::from_str::<Value>(qp_json_string().as_str()).unwrap()
        );
    }
}
