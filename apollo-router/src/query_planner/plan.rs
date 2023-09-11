use std::sync::Arc;

use router_bridge::planner::UsageReporting;
use serde::Deserialize;
use serde::Serialize;

pub(crate) use self::fetch::OperationKind;
use super::fetch;
use super::subscription::SubscriptionNode;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::spec::Query;

/// A planner key.
///
/// This type contains everything needed to separate query plan cache entries
#[derive(Clone)]
pub(crate) struct QueryKey {
    pub(crate) filtered_query: String,
    pub(crate) original_query: String,
    pub(crate) operation_name: Option<String>,
    pub(crate) metadata: CacheKeyMetadata,
}

/// A plan for a given GraphQL query
#[derive(Debug, Serialize, Deserialize)]
pub struct QueryPlan {
    pub(crate) usage_reporting: UsageReporting,
    pub(crate) root: PlanNode,
    /// String representation of the query plan (not a json representation)
    pub(crate) formatted_query_plan: Option<String>,
    pub(crate) query: Arc<Query>,
}

/// This default impl is useful for test users
/// who will need `QueryPlan`s to work with the `QueryPlannerService` and the `ExecutionService`
#[buildstructor::buildstructor]
impl QueryPlan {
    #[builder]
    pub(crate) fn fake_new(
        root: Option<PlanNode>,
        usage_reporting: Option<UsageReporting>,
    ) -> Self {
        Self {
            usage_reporting: usage_reporting.unwrap_or_else(|| UsageReporting {
                stats_report_key: "this is a test report key".to_string(),
                referenced_fields_by_type: Default::default(),
            }),
            root: root.unwrap_or_else(|| PlanNode::Sequence { nodes: Vec::new() }),
            formatted_query_plan: Default::default(),
            query: Arc::new(Query::empty()),
        }
    }
}

impl QueryPlan {
    pub(crate) fn is_deferred(&self, operation: Option<&str>, variables: &Object) -> bool {
        self.root.is_deferred(operation, variables, &self.query)
    }

    pub(crate) fn is_subscription(&self, operation: Option<&str>) -> bool {
        match self.query.operation(operation) {
            Some(op) => matches!(op.kind(), OperationKind::Subscription),
            None => false,
        }
    }
}

/// Query plans are composed of a set of nodes.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase", tag = "kind")]
pub(crate) enum PlanNode {
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
    Fetch(fetch::FetchNode),

    /// Merge the current resultset with the response.
    Flatten(FlattenNode),

    Defer {
        primary: Primary,
        deferred: Vec<DeferredNode>,
    },

    Subscription {
        primary: SubscriptionNode,
        rest: Option<Box<PlanNode>>,
    },

    #[serde(rename_all = "camelCase")]
    Condition {
        condition: String,
        if_clause: Option<Box<PlanNode>>,
        else_clause: Option<Box<PlanNode>>,
    },
}

impl PlanNode {
    pub(crate) fn contains_mutations(&self) -> bool {
        match self {
            Self::Sequence { nodes } => nodes.iter().any(|n| n.contains_mutations()),
            Self::Parallel { nodes } => nodes.iter().any(|n| n.contains_mutations()),
            Self::Fetch(fetch_node) => fetch_node.operation_kind() == &OperationKind::Mutation,
            Self::Defer { primary, .. } => primary
                .node
                .as_ref()
                .map(|n| n.contains_mutations())
                .unwrap_or(false),
            Self::Subscription { .. } => false,
            Self::Flatten(_) => false,
            Self::Condition {
                if_clause,
                else_clause,
                ..
            } => {
                if let Some(node) = if_clause {
                    if node.contains_mutations() {
                        return true;
                    }
                }
                if let Some(node) = else_clause {
                    if node.contains_mutations() {
                        return true;
                    }
                }
                false
            }
        }
    }

    pub(crate) fn is_deferred(
        &self,
        operation: Option<&str>,
        variables: &Object,
        query: &Query,
    ) -> bool {
        match self {
            Self::Sequence { nodes } => nodes
                .iter()
                .any(|n| n.is_deferred(operation, variables, query)),
            Self::Parallel { nodes } => nodes
                .iter()
                .any(|n| n.is_deferred(operation, variables, query)),
            Self::Flatten(node) => node.node.is_deferred(operation, variables, query),
            Self::Fetch(..) => false,
            Self::Defer { .. } => true,
            Self::Subscription { .. } => false,
            Self::Condition {
                if_clause,
                else_clause,
                condition,
            } => {
                if query
                    .variable_value(operation, condition.as_str(), variables)
                    .map(|v| *v == Value::Bool(true))
                    .unwrap_or(true)
                {
                    // right now ConditionNode is only used with defer, but it might be used
                    // in the future to implement @skip and @include execution
                    if let Some(node) = if_clause {
                        if node.is_deferred(operation, variables, query) {
                            return true;
                        }
                    }
                } else if let Some(node) = else_clause {
                    if node.is_deferred(operation, variables, query) {
                        return true;
                    }
                }

                false
            }
        }
    }

    pub(crate) fn subgraph_fetches(&self) -> usize {
        match self {
            PlanNode::Sequence { nodes } => nodes.iter().map(|n| n.subgraph_fetches()).sum(),
            PlanNode::Parallel { nodes } => nodes.iter().map(|n| n.subgraph_fetches()).sum(),
            PlanNode::Fetch(_) => 1,
            PlanNode::Flatten(node) => node.node.subgraph_fetches(),
            PlanNode::Defer { primary, deferred } => {
                primary.node.as_ref().map_or(0, |n| n.subgraph_fetches())
                    + deferred
                        .iter()
                        .map(|n| n.node.as_ref().map_or(0, |n| n.subgraph_fetches()))
                        .sum::<usize>()
            }
            // A `SubscriptionNode` makes a request to a subgraph, so counting it as 1
            PlanNode::Subscription { rest, .. } => {
                rest.as_ref().map_or(0, |n| n.subgraph_fetches()) + 1
            }
            // Compute the highest possible value for condition nodes
            PlanNode::Condition {
                if_clause,
                else_clause,
                ..
            } => std::cmp::max(
                if_clause
                    .as_ref()
                    .map(|n| n.subgraph_fetches())
                    .unwrap_or(0),
                else_clause
                    .as_ref()
                    .map(|n| n.subgraph_fetches())
                    .unwrap_or(0),
            ),
        }
    }

    #[cfg(test)]
    /// Retrieves all the services used across all plan nodes.
    ///
    /// Note that duplicates are not filtered.
    pub(crate) fn service_usage<'a>(&'a self) -> Box<dyn Iterator<Item = &'a str> + 'a> {
        match self {
            Self::Sequence { nodes } | Self::Parallel { nodes } => {
                Box::new(nodes.iter().flat_map(|x| x.service_usage()))
            }
            Self::Fetch(fetch) => Box::new(Some(fetch.service_name()).into_iter()),
            Self::Subscription { primary, rest } => match rest {
                Some(rest) => Box::new(
                    rest.service_usage()
                        .chain(Some(primary.service_name.as_str())),
                ) as Box<dyn Iterator<Item = &'a str> + 'a>,
                None => Box::new(Some(primary.service_name.as_str()).into_iter()),
            },
            Self::Flatten(flatten) => flatten.node.service_usage(),
            Self::Defer { primary, deferred } => primary
                .node
                .as_ref()
                .map(|n| {
                    Box::new(
                        n.service_usage().chain(
                            deferred
                                .iter()
                                .flat_map(|d| d.node.iter().flat_map(|node| node.service_usage())),
                        ),
                    ) as Box<dyn Iterator<Item = &'a str> + 'a>
                })
                .unwrap_or_else(|| {
                    Box::new(std::iter::empty()) as Box<dyn Iterator<Item = &'a str> + 'a>
                }),

            Self::Condition {
                if_clause,
                else_clause,
                ..
            } => match (if_clause, else_clause) {
                (None, None) => Box::new(None.into_iter()),
                (None, Some(node)) => node.service_usage(),
                (Some(node), None) => node.service_usage(),
                (Some(if_node), Some(else_node)) => {
                    Box::new(if_node.service_usage().chain(else_node.service_usage()))
                }
            },
        }
    }
}

/// A flatten node.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FlattenNode {
    /// The path when result should be merged.
    pub(crate) path: Path,

    /// The child execution plan.
    pub(crate) node: Box<PlanNode>,
}

/// A primary query for a Defer node, the non deferred part
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Primary {
    /// Optional path, set if and only if the defer node is a
    /// nested defer. If set, `subselection` starts at that `path`.
    pub(crate) path: Option<Path>,

    /// The part of the original query that "selects" the data to
    /// send in that primary response (once the plan in `node` completes).
    pub(crate) subselection: Option<String>,

    // The plan to get all the data for that primary part
    pub(crate) node: Option<Box<PlanNode>>,
}

/// The "deferred" parts of the defer (note that it's an array). Each
/// of those deferred elements will correspond to a different chunk of
/// the response to the client (after the initial non-deferred one that is).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeferredNode {
    /// References one or more fetch node(s) (by `id`) within
    /// `primary.node`. The plan of this deferred part should not
    /// be started before all those fetches returns.
    pub(crate) depends: Vec<Depends>,

    /// The optional defer label.
    pub(crate) label: Option<String>,
    /// Path to the @defer this correspond to. `subselection` start at that `path`.
    pub(crate) query_path: Path,
    /// The part of the original query that "selects" the data to send
    /// in that deferred response (once the plan in `node` completes).
    /// Will be set _unless_ `node` is a `DeferNode` itself.
    pub(crate) subselection: Option<String>,
    /// The plan to get all the data for that deferred part
    pub(crate) node: Option<Arc<PlanNode>>,
}

/// A deferred node.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Depends {
    pub(crate) id: String,
    pub(crate) defer_label: Option<String>,
}
