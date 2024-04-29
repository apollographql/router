use std::sync::Arc;

use apollo_compiler::executable;
use apollo_federation::query_plan as next;

use crate::query_planner::bridge_query_planner as bridge;
use crate::query_planner::fetch::SubgraphOperation;
use crate::query_planner::plan;
use crate::query_planner::rewrites;
use crate::query_planner::selection;
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
            next::TopLevelPlanNode::Condition(node) => node.into(),
        }
    }
}

impl From<&'_ next::PlanNode> for plan::PlanNode {
    fn from(value: &'_ next::PlanNode) -> Self {
        match value {
            next::PlanNode::Fetch(node) => node.as_ref().into(),
            next::PlanNode::Sequence(node) => node.as_ref().into(),
            next::PlanNode::Parallel(node) => node.as_ref().into(),
            next::PlanNode::Flatten(node) => node.as_ref().into(),
            next::PlanNode::Defer(node) => node.as_ref().into(),
            next::PlanNode::Condition(node) => node.as_ref().into(),
        }
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

impl From<&'_ next::FetchNode> for plan::PlanNode {
    fn from(value: &'_ next::FetchNode) -> Self {
        let next::FetchNode {
            subgraph_name,
            id,
            variable_usages,
            requires,
            operation_document,
            operation_name,
            operation_kind,
            input_rewrites,
            output_rewrites,
        } = value;
        Self::Fetch(super::fetch::FetchNode {
            service_name: subgraph_name.clone(),
            requires: vec(requires),
            variable_usages: variable_usages.iter().map(|v| v.clone().into()).collect(),
            // TODO: use Arc in apollo_federation to avoid this clone
            operation: SubgraphOperation::from_parsed(Arc::new(operation_document.clone())),
            operation_name: operation_name.clone(),
            operation_kind: (*operation_kind).into(),
            id: id.clone(),
            input_rewrites: option_vec(input_rewrites),
            output_rewrites: option_vec(output_rewrites),
            schema_aware_hash: Default::default(),
            authorization: Default::default(),
        })
    }
}

impl From<&'_ next::SequenceNode> for plan::PlanNode {
    fn from(value: &'_ next::SequenceNode) -> Self {
        let next::SequenceNode { nodes } = value;
        Self::Sequence { nodes: vec(nodes) }
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
            subgraph_name,
            id: _,
            variable_usages,
            requires: _,
            operation_document,
            operation_name,
            operation_kind,
            input_rewrites,
            output_rewrites,
        } = value;
        Self {
            service_name: subgraph_name.clone(),
            variable_usages: variable_usages.iter().map(|v| v.clone().into()).collect(),
            // TODO: use Arc in apollo_federation to avoid this clone
            operation: SubgraphOperation::from_parsed(Arc::new(operation_document.clone())),
            operation_name: operation_name.clone(),
            operation_kind: (*operation_kind).into(),
            input_rewrites: option_vec(input_rewrites),
            output_rewrites: option_vec(output_rewrites),
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

impl From<&'_ next::DeferredDeferBlock> for plan::DeferredNode {
    fn from(value: &'_ next::DeferredDeferBlock) -> Self {
        let next::DeferredDeferBlock {
            depends,
            label,
            query_path,
            sub_selection,
            node,
        } = value;
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

impl From<&'_ executable::Selection> for selection::Selection {
    fn from(value: &'_ executable::Selection) -> Self {
        match value {
            executable::Selection::Field(field) => Self::Field(field.as_ref().into()),
            executable::Selection::InlineFragment(inline) => {
                Self::InlineFragment(inline.as_ref().into())
            }
            executable::Selection::FragmentSpread(_) => unreachable!(),
        }
    }
}

impl From<&'_ executable::Field> for selection::Field {
    fn from(value: &'_ executable::Field) -> Self {
        let executable::Field {
            definition: _,
            alias,
            name,
            arguments: _,
            directives: _,
            selection_set,
        } = value;
        Self {
            alias: alias.clone(),
            name: name.clone(),
            selections: option_vec(&selection_set.selections),
        }
    }
}

impl From<&'_ executable::InlineFragment> for selection::InlineFragment {
    fn from(value: &'_ executable::InlineFragment) -> Self {
        let executable::InlineFragment {
            type_condition,
            directives: _,
            selection_set,
        } = value;
        Self {
            type_condition: type_condition.clone(),
            selections: vec(&selection_set.selections),
        }
    }
}

impl From<&'_ next::FetchDataRewrite> for rewrites::DataRewrite {
    fn from(value: &'_ next::FetchDataRewrite) -> Self {
        match value {
            next::FetchDataRewrite::ValueSetter(setter) => Self::ValueSetter(setter.into()),
            next::FetchDataRewrite::KeyRenamer(renamer) => Self::KeyRenamer(renamer.into()),
        }
    }
}

impl From<&'_ next::FetchDataValueSetter> for rewrites::DataValueSetter {
    fn from(value: &'_ next::FetchDataValueSetter) -> Self {
        let next::FetchDataValueSetter { path, set_value_to } = value;
        Self {
            path: crate::json_ext::Path(vec(path)),
            set_value_to: set_value_to.clone(),
        }
    }
}

impl From<&'_ next::FetchDataKeyRenamer> for rewrites::DataKeyRenamer {
    fn from(value: &'_ next::FetchDataKeyRenamer) -> Self {
        let next::FetchDataKeyRenamer {
            path,
            rename_key_to,
        } = value;
        Self {
            path: crate::json_ext::Path(vec(path)),
            rename_key_to: rename_key_to.clone(),
        }
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

fn option_vec<'a, T, U>(value: &'a [T]) -> Option<Vec<U>>
where
    U: From<&'a T>,
{
    if value.is_empty() {
        None
    } else {
        Some(vec(value))
    }
}
