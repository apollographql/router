use std::sync::Arc;

use apollo_federation::source_aware::query_plan as next;

use crate::query_planner::bridge_query_planner as bridge;
use crate::query_planner::fetch::SubgraphOperation;
use crate::query_planner::plan;
use crate::query_planner::subscription;

impl From<&'_ next::QueryPlan> for bridge::QueryPlan {
    fn from(value: &'_ next::QueryPlan) -> Self {
        let next::QueryPlan { node } = value;
        Self { node: option(node) }
    }
}

impl From<&'_ next::TopLevelPlanNode> for plan::PlanNode {
    fn from(value: &'_ next::TopLevelPlanNode) -> Self {
        match value {
            next::TopLevelPlanNode::Subscription(node) => node.into(),
            next::TopLevelPlanNode::Fetch(node) => node.into(),
            next::TopLevelPlanNode::Sequence(node) => node.into(),
            next::TopLevelPlanNode::Parallel(node) => node.into(),
            next::TopLevelPlanNode::Flatten(node) => node.into(),
            next::TopLevelPlanNode::Defer(node) => node.into(),
            next::TopLevelPlanNode::Condition(node) => node.as_ref().into(),
        }
    }
}

impl From<&'_ next::PlanNode> for plan::PlanNode {
    fn from(value: &'_ next::PlanNode) -> Self {
        match value {
            next::PlanNode::Fetch(node) => node.into(),
            next::PlanNode::Sequence(node) => node.into(),
            next::PlanNode::Parallel(node) => node.into(),
            next::PlanNode::Flatten(node) => node.into(),
            next::PlanNode::Defer(node) => node.into(),
            next::PlanNode::Condition(node) => node.as_ref().into(),
        }
    }
}

impl From<&'_ Box<next::PlanNode>> for plan::PlanNode {
    fn from(value: &'_ Box<next::PlanNode>) -> Self {
        value.as_ref().into()
    }
}

impl From<&'_ next::SubscriptionNode> for plan::PlanNode {
    fn from(value: &'_ next::SubscriptionNode) -> Self {
        let next::SubscriptionNode { primary, rest } = value;
        Self::Subscription {
            primary: primary.into(),
            rest: option(rest).map(Box::new),
        }
    }
}

impl From<&'_ Box<next::FetchNode>> for plan::PlanNode {
    fn from(value: &'_ Box<next::FetchNode>) -> Self {
        let next::FetchNode {
            operation_variables: _,
            input_conditions: _,
            source_data,
        } = value.as_ref();

        match source_data {
            apollo_federation::sources::source::query_plan::FetchNode::Graphql(fetch) => {
                Self::Fetch(super::fetch::FetchNode {
                    service_name: fetch.source_id.subgraph_name.clone(),
                    requires: Default::default(),        // TODO
                    variable_usages: Default::default(), // TODO
                    operation: SubgraphOperation::from_parsed(Arc::new(
                        fetch.operation_document.clone(),
                    )),
                    operation_name: fetch.operation_name.clone(),
                    operation_kind: fetch.operation_kind.into(),
                    id: Default::default(),               // TODO
                    input_rewrites: Default::default(),   // TODO
                    output_rewrites: Default::default(),  // TODO
                    context_rewrites: Default::default(), // TODO
                    schema_aware_hash: Default::default(),
                    authorization: Default::default(),
                    protocol: Default::default(),
                    source_node: None, // Ignored
                })
            }
            apollo_federation::sources::source::query_plan::FetchNode::Connect(fetch) => {
                Self::Fetch(super::fetch::FetchNode {
                    service_name: fetch.source_id.subgraph_name.clone(), // Ignored
                    requires: Default::default(),                        // TODO/Ignored?
                    variable_usages: Default::default(),                 // TODO/Ignored?
                    operation: SubgraphOperation::from_string("{__typename}"), // Ignored
                    operation_name: Default::default(),                  // Ignored
                    operation_kind: Default::default(),                  // Ignored
                    id: Default::default(),                              // TODO/Ignored?
                    input_rewrites: Default::default(),                  // TODO/Ignored?
                    output_rewrites: Default::default(),                 // TODO/Ignored?
                    context_rewrites: Default::default(),                // TODO/Ignored?
                    schema_aware_hash: Default::default(),
                    authorization: Default::default(),
                    protocol: Default::default(),
                    source_node: Some(Arc::new(source_data.clone())),
                })
            }
        }
    }
}

impl From<&'_ next::SequenceNode> for plan::PlanNode {
    fn from(value: &'_ next::SequenceNode) -> Self {
        let next::SequenceNode { nodes } = value;
        Self::Sequence {
            nodes: vec(nodes),
            connector: Default::default(),
        }
    }
}

impl From<&'_ next::ParallelNode> for plan::PlanNode {
    fn from(value: &'_ next::ParallelNode) -> Self {
        let next::ParallelNode { nodes } = value;
        Self::Parallel { nodes: vec(nodes) }
    }
}

impl From<&'_ next::FlattenNode> for plan::PlanNode {
    fn from(value: &'_ next::FlattenNode) -> Self {
        let next::FlattenNode { path, node } = value;
        Self::Flatten(plan::FlattenNode {
            path: crate::json_ext::Path(vec(path)),
            node: Box::new(node.into()),
        })
    }
}

impl From<&'_ next::DeferNode> for plan::PlanNode {
    fn from(value: &'_ next::DeferNode) -> Self {
        let next::DeferNode { primary, deferred } = value;
        Self::Defer {
            primary: primary.into(),
            deferred: vec(deferred),
        }
    }
}

impl From<&'_ next::ConditionNode> for plan::PlanNode {
    fn from(value: &'_ next::ConditionNode) -> Self {
        let next::ConditionNode {
            condition_variable,
            if_clause,
            else_clause,
        } = value;
        Self::Condition {
            condition: condition_variable.to_string(),
            if_clause: if_clause.as_ref().map(Into::into).map(Box::new),
            else_clause: else_clause.as_ref().map(Into::into).map(Box::new),
        }
    }
}

impl From<&'_ next::FetchNode> for subscription::SubscriptionNode {
    fn from(value: &'_ next::FetchNode) -> Self {
        let next::FetchNode {
            operation_variables: _,
            input_conditions: _,
            source_data,
        } = value;

        match source_data {
            apollo_federation::sources::source::query_plan::FetchNode::Graphql(fetch) => Self {
                service_name: fetch.source_id.subgraph_name.clone(),
                variable_usages: Default::default(), // TODO variable_usages.iter().map(|v| v.clone().into()).collect(),
                // TODO: use Arc in apollo_federation to avoid this clone
                operation: SubgraphOperation::from_parsed(Arc::new(
                    fetch.operation_document.clone(),
                )),
                operation_name: fetch.operation_name.clone(),
                operation_kind: fetch.operation_kind.into(),
                input_rewrites: Default::default(), // TODO option_vec(input_rewrites),
                output_rewrites: Default::default(), // option_vec(output_rewrites),
            },
            apollo_federation::sources::source::query_plan::FetchNode::Connect(_) => {
                panic!("no subscriptions for connectors")
            }
        }
    }
}

impl From<&'_ next::PrimaryDeferBlock> for plan::Primary {
    fn from(value: &'_ next::PrimaryDeferBlock) -> Self {
        let next::PrimaryDeferBlock {
            sub_selection,
            node,
        } = value;
        Self {
            node: option(node).map(Box::new),
            subselection: sub_selection.as_ref().map(|s| s.to_string()),
        }
    }
}

impl From<&'_ Box<next::DeferredDeferBlock>> for plan::DeferredNode {
    fn from(value: &'_ Box<next::DeferredDeferBlock>) -> Self {
        let next::DeferredDeferBlock {
            depends,
            label,
            query_path,
            sub_selection,
            node,
        } = value.as_ref();
        Self {
            depends: vec(depends),
            label: label.clone(),
            query_path: crate::json_ext::Path(
                query_path
                    .iter()
                    .filter_map(|e| match e {
                        next::QueryPathElement::Field(field) => Some(
                            // TODO: type conditioned fetching once it s available in the rust planner
                            crate::graphql::JsonPathElement::Key(
                                field.response_key().to_string(),
                                None,
                            ),
                        ),
                        next::QueryPathElement::InlineFragment(inline) => {
                            inline.type_condition.as_ref().map(|cond| {
                                crate::graphql::JsonPathElement::Fragment(cond.to_string())
                            })
                        }
                    })
                    .collect(),
            ),
            node: option(node).map(Arc::new),
            subselection: sub_selection.as_ref().map(|s| s.to_string()),
        }
    }
}

impl From<&'_ next::DeferredDependency> for plan::Depends {
    fn from(value: &'_ next::DeferredDependency) -> Self {
        let next::DeferredDependency { id } = value;
        Self { id: id.clone() }
    }
}

impl From<&'_ next::FetchDataPathElement> for crate::json_ext::PathElement {
    fn from(value: &'_ next::FetchDataPathElement) -> Self {
        match value {
            // TODO: Type conditioned fetching once it's available in the rust planner
            next::FetchDataPathElement::Key(value) => Self::Key(value.to_string(), None),
            next::FetchDataPathElement::AnyIndex => Self::Flatten(None),
            next::FetchDataPathElement::TypenameEquals(value) => Self::Fragment(value.to_string()),
        }
    }
}

fn vec<'a, T, U>(value: &'a [T]) -> Vec<U>
where
    U: From<&'a T>,
{
    value.iter().map(Into::into).collect()
}

fn option<'a, T, U>(value: &'a Option<T>) -> Option<U>
where
    U: From<&'a T>,
{
    value.as_ref().map(Into::into)
}
