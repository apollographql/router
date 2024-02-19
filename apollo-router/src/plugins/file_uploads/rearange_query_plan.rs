use std::cmp;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;

use indexmap::IndexSet;
use itertools::Itertools;

use super::error::FileUploadError;
use super::MapPerVariable;
use super::UploadResult;
use crate::query_planner::DeferredNode;
use crate::query_planner::FlattenNode;
use crate::query_planner::PlanNode;
use crate::services::execution::QueryPlan;

pub(super) fn rearange_query_plan(
    query_plan: &QueryPlan,
    files_order: &IndexSet<String>,
    map_per_variable: &MapPerVariable,
) -> UploadResult<QueryPlan> {
    let root = &query_plan.root;
    let mut variable_ranges = HashMap::new();
    for (name, map) in map_per_variable.iter() {
        variable_ranges.insert(
            name.as_str(),
            map.keys()
                .map(|file| files_order.get_index_of(file))
                .minmax()
                .into_option()
                .expect("map always have keys"),
        );
    }

    let root = rearrange_plan_node(root, &mut HashMap::new(), &variable_ranges)?;
    Ok(QueryPlan {
        root,
        usage_reporting: query_plan.usage_reporting.clone(),
        formatted_query_plan: query_plan.formatted_query_plan.clone(),
        query: query_plan.query.clone(),
    })
}

// Recursive, and recursion is safe here since query plan is executed recursively.
fn rearrange_plan_node<'a>(
    node: &PlanNode,
    acc_variables: &mut HashMap<&'a str, &'a (Option<usize>, Option<usize>)>,
    variable_ranges: &'a HashMap<&str, (Option<usize>, Option<usize>)>,
) -> UploadResult<PlanNode> {
    Ok(match node {
        PlanNode::Condition {
            condition,
            if_clause,
            else_clause,
        } => {
            let if_clause = if_clause
                .as_ref()
                .map(|node| rearrange_plan_node(&node, acc_variables, variable_ranges))
                .transpose();

            let else_clause = else_clause
                .as_ref()
                .map(|node| rearrange_plan_node(&node, acc_variables, variable_ranges))
                .transpose();

            PlanNode::Condition {
                condition: condition.clone(),
                if_clause: if_clause?.map(Box::new),
                else_clause: else_clause?.map(Box::new),
            }
        }
        PlanNode::Fetch(fetch) => {
            for variable in fetch.variable_usages.iter() {
                if let Some((name, range)) = variable_ranges.get_key_value(variable.as_str()) {
                    acc_variables.entry(name).or_insert(range);
                }
            }
            PlanNode::Fetch(fetch.clone())
        }
        PlanNode::Subscription { primary, rest } => {
            for variable in primary.variable_usages.iter() {
                if let Some((name, range)) = variable_ranges.get_key_value(variable.as_str()) {
                    acc_variables.entry(name).or_insert(range);
                }
            }

            if let Some(rest) = rest {
                let mut rest_variables = HashMap::new();
                // ignore result use it just to collect variables
                drop(rearrange_plan_node(
                    rest,
                    &mut rest_variables,
                    variable_ranges,
                ));
                if !rest_variables.is_empty() {
                    return Err(FileUploadError::VariablesForbiddenInsideSubscription(
                        rest_variables
                            .into_keys()
                            .map(|name| format!("${}", name))
                            .join(", "),
                    ));
                }
            }

            PlanNode::Subscription {
                primary: primary.clone(),
                rest: rest.clone(),
            }
        }
        PlanNode::Defer { primary, deferred } => {
            let mut primary = primary.clone();
            let deferred = deferred.clone();

            let primary_node = primary
                .node
                .map(|node| rearrange_plan_node(&node, acc_variables, variable_ranges))
                .transpose();

            let mut deferred_variables = HashMap::new();
            for DeferredNode { node, .. } in deferred.iter() {
                if let Some(node) = node {
                    // ignore result use it just to collect variables
                    drop(rearrange_plan_node(
                        node,
                        &mut deferred_variables,
                        variable_ranges,
                    ));
                }
            }
            if !deferred_variables.is_empty() {
                return Err(FileUploadError::VariablesForbiddenInsideDefer(
                    deferred_variables
                        .into_keys()
                        .map(|name| format!("${}", name))
                        .join(", "),
                ));
            }

            primary.node = primary_node?.map(Box::new);
            PlanNode::Defer { primary, deferred }
        }
        PlanNode::Flatten(flatten_node) => {
            let node = rearrange_plan_node(&flatten_node.node, acc_variables, variable_ranges)?;
            PlanNode::Flatten(FlattenNode {
                node: Box::new(node),
                path: flatten_node.path.clone(),
            })
        }
        PlanNode::Sequence { nodes } => {
            let mut sequence = Vec::new();
            let mut sequence_last = None;

            let mut has_overlap = false;
            let mut duplicate_variables = HashSet::new();
            for node in nodes.iter() {
                let mut node_variables = HashMap::new();
                let node = rearrange_plan_node(node, &mut node_variables, variable_ranges)?;
                sequence.push(node);

                for (variable, range) in node_variables.into_iter() {
                    if acc_variables.insert(variable, range).is_some() {
                        duplicate_variables.insert(variable);
                        continue;
                    }

                    let (first, last) = range;
                    if *first <= sequence_last {
                        has_overlap = true;
                    }
                    sequence_last = *last;
                }
            }

            if !duplicate_variables.is_empty() {
                return Err(FileUploadError::DuplicateVariableUsages(
                    duplicate_variables
                        .iter()
                        .map(|name| format!("${}", name))
                        .join(", "),
                ));
            }
            if has_overlap {
                return Err(FileUploadError::MisorderedVariables);
            }
            PlanNode::Sequence { nodes: sequence }
        }
        PlanNode::Parallel { nodes } => {
            let mut parallel = Vec::new();
            let mut sequence = BTreeMap::new();
            let mut duplicate_variables = HashSet::new();

            for node in nodes.iter() {
                let mut node_variables = HashMap::new();
                let node = rearrange_plan_node(node, &mut node_variables, variable_ranges)?;
                if node_variables.is_empty() {
                    parallel.push(node);
                    continue;
                }

                let mut first_file = None;
                let mut last_file = None;
                for (variable, range) in node_variables.into_iter() {
                    if acc_variables.insert(variable, range).is_some() {
                        duplicate_variables.insert(variable);
                        continue;
                    }

                    let (first, last) = range;
                    first_file = match first_file {
                        None => *first,
                        Some(first_file) => cmp::min(Some(first_file), *first),
                    };
                    last_file = cmp::max(last_file, *last);
                }
                sequence.insert(first_file, (node, last_file));
            }

            if !duplicate_variables.is_empty() {
                return Err(FileUploadError::DuplicateVariableUsages(
                    duplicate_variables
                        .iter()
                        .map(|name| format!("${}", name))
                        .join(", "),
                ));
            }

            if !sequence.is_empty() {
                let mut nodes = Vec::new();
                let mut sequence_last_file = None;
                for (first_file, (node, last_file)) in sequence.into_iter() {
                    if first_file <= sequence_last_file {
                        return Err(FileUploadError::MisorderedVariables);
                    }
                    sequence_last_file = last_file;
                    nodes.push(node);
                }

                parallel.push(PlanNode::Sequence { nodes });
            }

            PlanNode::Parallel { nodes: parallel }
        }
    })
}
