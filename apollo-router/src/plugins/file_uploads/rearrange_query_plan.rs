use std::cmp;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;

use itertools::Itertools;

use super::error::FileUploadError;
use super::MapField;
use super::Result as UploadResult;
use crate::query_planner::DeferredNode;
use crate::query_planner::FlattenNode;
use crate::query_planner::PlanNode;
use crate::services::execution::QueryPlan;

/// In order to avoid deadlocks, we need to make sure files streamed to subgraphs
/// Are streamed in the order the client sent.
/// Sometimes we can't achieve that, so we return an error.
// TODO: This needs to be moved, possibly to a query planner validation step eventually.
// Change order of nodes inside QueryPlan to follow order of files in client's request
pub(super) fn rearrange_query_plan(
    query_plan: &QueryPlan,
    map: &MapField,
) -> UploadResult<QueryPlan> {
    let root = &query_plan.root;
    let mut variable_ranges = HashMap::with_capacity(map.per_variable.len());
    for (name, submap) in map.per_variable.iter() {
        variable_ranges.insert(
            name.as_str(),
            submap
                .keys()
                .map(|file| map.files_order.get_index_of(file))
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

// Recursive, and recursion is safe here since query plan is also executed recursively.
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
            // Rearrange and validate nodes inside 'if_clause'
            let if_clause = if_clause
                .as_ref()
                .map(|node| rearrange_plan_node(node, acc_variables, variable_ranges))
                .transpose();

            // Rearrange and validate nodes inside 'if_clause'
            let else_clause = else_clause
                .as_ref()
                .map(|node| rearrange_plan_node(node, acc_variables, variable_ranges))
                .transpose();

            PlanNode::Condition {
                condition: condition.clone(),
                if_clause: if_clause?.map(Box::new),
                else_clause: else_clause?.map(Box::new),
            }
        }
        PlanNode::Fetch(fetch) => {
            // Extract variables used in this node.
            for variable in fetch.variable_usages.iter() {
                if let Some((name, range)) = variable_ranges.get_key_value(variable.as_str()) {
                    acc_variables.entry(name).or_insert(range);
                }
            }
            PlanNode::Fetch(fetch.clone())
        }
        PlanNode::Subscription { primary, rest } => {
            // Extract variables used in this node
            for variable in primary.variable_usages.iter() {
                if let Some((name, range)) = variable_ranges.get_key_value(variable.as_str()) {
                    acc_variables.entry(name).or_insert(range);
                }
            }

            // Error if 'rest' contains file variables
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

            // Rearrange and validate nodes inside 'primary'
            let primary_node = primary
                .node
                .map(|node| rearrange_plan_node(&node, acc_variables, variable_ranges))
                .transpose();

            // Error if 'deferred' contains file variables
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
            // Rearrange and validate nodes inside 'flatten_node'
            let node = rearrange_plan_node(&flatten_node.node, acc_variables, variable_ranges)?;
            PlanNode::Flatten(FlattenNode {
                node: Box::new(node),
                path: flatten_node.path.clone(),
            })
        }
        PlanNode::Sequence { nodes } => {
            // We can't rearange nodes inside a Sequence so just error if "file ranges" of nodes overlaps.
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
                        // To improve DX we also tracking duplicating variables as separate error.
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
            // We can rearange nodes inside a Parallel, so we order all nodes based on the first file they use and wrap them into Sequence node.
            // Note: we don't wrap or change order of nodes that don't use "file variables".
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
                        // To improve DX we also tracking duplicating variables as separate error.
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
                // Nodes are sorted inside 'sequence' based on the 'first_file'
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

#[test]
fn test_rearrange_impossible_plan() {
    let root = serde_json::from_str(r#"{
        "kind": "Sequence",
        "nodes": [
          {
            "kind": "Fetch",
            "serviceName": "uploads1",
            "variableUsages": [
              "file1"
            ],
            "operation": "mutation SomeMutation__uploads1__0($file1:Upload1){file1:singleUpload1(file:$file1){filename}}",
            "operationName": "SomeMutation__uploads1__0",
            "operationKind": "mutation",
            "id": null,
            "inputRewrites": null,
            "outputRewrites": null,
            "schemaAwareHash": "0239133f4bf1e52ed2d84a06563d98d61a197ec417490a38b37aaeecd98b315c",
            "authorization": {
              "is_authenticated": false,
              "scopes": [],
              "policies": []
            }
          },
          {
            "kind": "Fetch",
            "serviceName": "uploads2",
            "variableUsages": [
              "file0"
            ],
            "operation": "mutation SomeMutation__uploads2__1($file0:Upload2){file0:singleUpload2(file:$file0){filename}}",
            "operationName": "SomeMutation__uploads2__1",
            "operationKind": "mutation",
            "id": null,
            "inputRewrites": null,
            "outputRewrites": null,
            "schemaAwareHash": "41fda639a3b69227226d234fed29d63124e0a95ac9ff98c611e903d4b2adcd8c",
            "authorization": {
              "is_authenticated": false,
              "scopes": [],
              "policies": []
            }
          }
        ]
      }"#).unwrap();

    let variable_ranges =
        HashMap::from([("file1", (Some(1), Some(1))), ("file0", (Some(0), Some(0)))]);
    let root = rearrange_plan_node(&root, &mut HashMap::new(), &variable_ranges).unwrap_err();
    assert_eq!("References to variables containing files are ordered in the way that prevent streaming of files.".to_string(), root.to_string());
}
