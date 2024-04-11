use std::cmp;
use std::collections::BTreeMap;
use std::collections::HashMap;

use indexmap::IndexMap;
use indexmap::IndexSet;
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

    let root = rearrange_plan_node(root, &mut IndexMap::new(), &variable_ranges)?;
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
    acc_variables: &mut IndexMap<&'a str, &'a (Option<usize>, Option<usize>)>,
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
                let mut rest_variables = IndexMap::new();
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
            let mut deferred_variables = IndexMap::new();
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
            let mut duplicate_variables = IndexSet::new();
            for node in nodes.iter() {
                let mut node_variables = IndexMap::new();
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
            let mut duplicate_variables = IndexSet::new();

            for node in nodes.iter() {
                let mut node_variables = IndexMap::new();
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

            if sequence.len() <= 1 {
                // if there are no node competing for files, keep nodes nodes in Parallel
                parallel.extend(sequence.into_values().map(|(node, _)| node));
                PlanNode::Parallel { nodes: parallel }
            } else {
                let mut nodes = Vec::new();
                let mut sequence_last_file = None;
                for (first_file, (node, last_file)) in sequence.into_iter() {
                    if first_file <= sequence_last_file {
                        return Err(FileUploadError::MisorderedVariables);
                    }
                    sequence_last_file = last_file;
                    nodes.push(node);
                }

                if parallel.is_empty() {
                    // if all nodes competing for files replace Parallel with Sequence
                    PlanNode::Sequence { nodes }
                } else {
                    // if some of the nodes competing for files wrap them with Sequence within Parallel
                    parallel.push(PlanNode::Sequence { nodes });
                    PlanNode::Parallel { nodes: parallel }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use indexmap::indexmap;
    use serde_json::json;

    use super::*;
    use crate::query_planner::subscription::SubscriptionNode;
    use crate::query_planner::Primary;
    use crate::services::execution::QueryPlan;

    // Custom `assert_matches` due to its current nightly-only status, see
    // https://github.com/rust-lang/rust/issues/82775
    macro_rules! assert_matches {
        ($actual:expr, $(|)? $( $pattern:pat_param )|+ $( if $guard: expr )? $(,)?) => {
            let result = $actual;
            assert!(
                matches!(result, $( $pattern )|+ $( if $guard )?),
                "got {:?} but expected {:?}",
                result,
                "", // stringify!($pattern)
            );
        };
    }

    fn fake_query_plan(root_json: serde_json::Value) -> QueryPlan {
        QueryPlan::fake_new(Some(serde_json::from_value(root_json).unwrap()), None)
    }

    fn to_root_json(query_plan: QueryPlan) -> serde_json::Value {
        serde_json::to_value(query_plan.root).unwrap()
    }

    fn normalize_json<T: serde::de::DeserializeOwned + serde::ser::Serialize>(
        json: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::to_value(serde_json::from_value::<T>(json).unwrap()).unwrap()
    }

    fn fake_fetch(service_name: &str, variables: Vec<&str>) -> serde_json::Value {
        normalize_json::<PlanNode>(json!({
          "kind": "Fetch",
          "serviceName": service_name.to_owned(),
          "variableUsages": variables.to_owned(),
          "operation": "",
          "operationKind": "query"
        }))
    }

    fn fake_subscription(service_name: &str, variables: Vec<&str>) -> serde_json::Value {
        normalize_json::<SubscriptionNode>(json!({
          "serviceName": service_name.to_owned(),
          "variableUsages": variables.to_owned(),
          "operation": "",
          "operationKind": "subscription"
        }))
    }

    fn fake_primary(node: serde_json::Value) -> serde_json::Value {
        normalize_json::<Primary>(json!({ "node": node }))
    }

    fn fake_deferred(node: serde_json::Value) -> serde_json::Value {
        normalize_json::<DeferredNode>(json!({
          "depends": [],
          "queryPath": [],
          "node": node,
        }))
    }

    #[test]
    fn test_valid_conditional_node() {
        let root_json = json!({
          "kind": "Condition",
          "condition": "",
          "ifClause": fake_fetch("uploads1", vec!["file"]),
          "elseClause":  fake_fetch("uploads2", vec!["file"]),
        });
        let query_plan = fake_query_plan(root_json.clone());

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_eq!(to_root_json(result.unwrap()), root_json);
    }

    #[test]
    fn test_inner_error_within_conditional_node() {
        let query_plan = fake_query_plan(json!({
          "kind": "Condition",
          "condition": "",
          "ifClause": {
            "kind": "Sequence",
            "nodes": [
              fake_fetch("uploads1", vec!["file2"]),
              fake_fetch("uploads2", vec!["file1"])
            ]
          }
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file1".to_owned()],
            "1".to_owned() => vec!["variables.file2".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_matches!(result, Err(FileUploadError::MisorderedVariables));
    }

    #[test]
    fn test_conditional_node_overlapping_with_external_node() {
        let query_plan = fake_query_plan(json!({
          "kind": "Sequence",
          "nodes": [
            {
              "kind": "Condition",
              "condition": "",
              "ifClause": fake_fetch("uploads1", vec!["file"]),
              "elseClause":  fake_fetch("uploads2", vec!["file"]),
            },
            fake_fetch("uploads3", vec!["file"]),
          ]
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_matches!(
            result,
            Err(FileUploadError::DuplicateVariableUsages(ref variables)) if variables == "$file",
        );
    }

    #[test]
    fn test_valid_subscription_node() {
        let root_json = json!({
          "kind": "Subscription",
          "primary": fake_subscription("uploads", vec!["file"]),
          "rest":  fake_fetch("subgraph", vec!["not_a_file"]),
        });
        let query_plan = fake_query_plan(root_json.clone());

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_eq!(to_root_json(result.unwrap()), root_json);
    }

    #[test]
    fn test_valid_file_inside_of_subscription_rest() {
        let query_plan = fake_query_plan(json!({
          "kind": "Subscription",
          "primary": fake_subscription("uploads1", vec!["file2"]),
          "rest":  {
            "kind": "Sequence",
            "nodes": [
              // Note: order is invalid on purpose since we are testing that user get
              // error about variables inside subscription instead of internal error.
              fake_fetch("uploads1", vec!["file2"]),
              fake_fetch("uploads2", vec!["file1"])
            ]
           }
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file1".to_owned()],
            "1".to_owned() => vec!["variables.file2".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_matches!(
            result,
            Err(FileUploadError::VariablesForbiddenInsideSubscription(ref variables)) if variables == "$file2, $file1",
        );
    }

    #[test]
    fn test_valid_defer_node() {
        let root_json = json!({
          "kind": "Defer",
          "primary": fake_primary(fake_fetch("uploads", vec!["file"])),
          "deferred":  [fake_deferred(fake_fetch("subgraph", vec!["not_a_file"]))],
        });
        let query_plan = fake_query_plan(root_json.clone());

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_eq!(to_root_json(result.unwrap()), root_json);
    }

    #[test]
    fn test_file_inside_of_deffered() {
        let query_plan = fake_query_plan(json!({
          "kind": "Defer",
          "primary": fake_primary(fake_fetch("uploads", vec!["file"])),
          "deferred":  [
              fake_deferred(json!({
                "kind": "Sequence",
                "nodes": [
                  // Note: order is invalid on purpose since we are testing that user get
                  // error about variables inside deffered instead of internal error.
                  fake_fetch("uploads1", vec!["file2"]),
                  fake_fetch("uploads2", vec!["file1"])
                ]
              }))
          ],
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file1".to_owned()],
            "1".to_owned() => vec!["variables.file2".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_matches!(
            result,
            Err(FileUploadError::VariablesForbiddenInsideDefer(ref variables)) if variables == "$file2, $file1",
        );
    }

    #[test]
    fn test_inner_error_within_defer_node() {
        let query_plan = fake_query_plan(json!({
          "kind": "Defer",
          "primary": fake_primary(json!({
            "kind": "Sequence",
            "nodes": [
              fake_fetch("uploads1", vec!["file2"]),
              fake_fetch("uploads2", vec!["file1"])
            ]
          })),
          "deferred":  []
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file1".to_owned()],
            "1".to_owned() => vec!["variables.file2".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_matches!(result, Err(FileUploadError::MisorderedVariables));
    }

    #[test]
    fn test_defer_node_overlapping_with_external_node() {
        let query_plan = fake_query_plan(json!({
          "kind": "Sequence",
          "nodes": [
            {
              "kind": "Defer",
              "primary": fake_primary(json!(fake_fetch("uploads1", vec!["file"]))),
              "deferred":  []
            },
            fake_fetch("uploads2", vec!["file"]),
          ]
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_matches!(
            result,
            Err(FileUploadError::DuplicateVariableUsages(ref variables)) if variables == "$file",
        );
    }

    #[test]
    fn test_valid_flatten_node() {
        let root_json = json!({
          "kind": "Flatten",
          "path": [],
          "node": fake_fetch("uploads", vec!["file"]),
        });
        let query_plan = fake_query_plan(root_json.clone());

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_eq!(to_root_json(result.unwrap()), root_json);
    }

    #[test]
    fn test_inner_error_within_flatten_node() {
        let query_plan = fake_query_plan(json!({
          "kind": "Flatten",
          "path": [],
          "node": {
            "kind": "Sequence",
            "nodes": [
              fake_fetch("uploads1", vec!["file2"]),
              fake_fetch("uploads2", vec!["file1"])
            ]
          },
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file1".to_owned()],
            "1".to_owned() => vec!["variables.file2".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_matches!(result, Err(FileUploadError::MisorderedVariables));
    }

    #[test]
    fn test_flatten_node_overlapping_with_external_node() {
        let query_plan = fake_query_plan(json!({
          "kind": "Sequence",
          "nodes": [
            {
              "kind": "Flatten",
              "path": [],
              "node": fake_fetch("uploads1", vec!["file"]),
            },
            fake_fetch("uploads2", vec!["file"]),
          ]
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_matches!(
            result,
            Err(FileUploadError::DuplicateVariableUsages(ref variables)) if variables == "$file",
        );
    }

    #[test]
    fn test_valid_sequence() {
        let root_json = json!({
          "kind": "Sequence",
          "nodes": [
            fake_fetch("uploads1", vec!["file1"]),
            fake_fetch("uploads2", vec!["file2"])
          ]
        });
        let query_plan = fake_query_plan(root_json.clone());

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file1".to_owned()],
            "1".to_owned() => vec!["variables.file2".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_eq!(to_root_json(result.unwrap()), root_json);
    }

    #[test]
    fn test_missordered_sequence() {
        let query_plan = fake_query_plan(json!({
          "kind": "Sequence",
          "nodes": [
            fake_fetch("uploads1", vec!["file2"]),
            fake_fetch("uploads2", vec!["file1"])
          ]
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file1".to_owned()],
            "1".to_owned() => vec!["variables.file2".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_matches!(result, Err(FileUploadError::MisorderedVariables));
    }

    #[test]
    fn test_sequence_with_overlapping_variables() {
        let query_plan = fake_query_plan(json!({
          "kind": "Sequence",
          "nodes": [
            fake_fetch("uploads1", vec!["files1"]),
            fake_fetch("uploads2", vec!["files2"])
          ]
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.files1.0".to_owned()],
            "1".to_owned() => vec!["variables.files2.0".to_owned()],
            "2".to_owned() => vec!["variables.files1.1".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_matches!(result, Err(FileUploadError::MisorderedVariables));
    }

    #[test]
    fn test_sequence_with_duplicated_variables() {
        let query_plan = fake_query_plan(json!({
          "kind": "Sequence",
          "nodes": [
            fake_fetch("uploads1", vec!["file1"]),
            fake_fetch("uploads2", vec!["file2", "file3"]),
            fake_fetch("uploads3", vec!["file1"]),
            fake_fetch("uploads4", vec!["file2", "file4"])
          ]
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file1".to_owned()],
            "1".to_owned() => vec!["variables.file2".to_owned()],
            "2".to_owned() => vec!["variables.file3".to_owned()],
            "3".to_owned() => vec!["variables.file4".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_matches!(
            result,
            Err(FileUploadError::DuplicateVariableUsages(ref variables)) if variables == "$file1, $file2",
        );
    }

    #[test]
    fn test_keep_nodes_in_parallel() {
        let query_plan = fake_query_plan(json!({
          "kind": "Parallel",
          "nodes": [
            fake_fetch("subgraph1", vec!["not_a_file"]),
            fake_fetch("subgraph2", vec!["not_a_file"]),
            fake_fetch("uploads1", vec!["file1"]),
          ]
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file1".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_eq!(
            to_root_json(result.unwrap()),
            json!({
              "kind": "Parallel",
              "nodes": [
                fake_fetch("subgraph1", vec!["not_a_file"]),
                fake_fetch("subgraph2", vec!["not_a_file"]),
                fake_fetch("uploads1", vec!["file1"]),
              ]
            })
        );
    }

    #[test]
    fn test_convert_parallel_to_sequence() {
        let query_plan = fake_query_plan(json!({
          "kind": "Parallel",
          "nodes": [
            fake_fetch("uploads1", vec!["file1"]),
            fake_fetch("uploads2", vec!["file2"])
          ]
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file1".to_owned()],
            "1".to_owned() => vec!["variables.file2".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_eq!(
            to_root_json(result.unwrap()),
            json!({
              "kind": "Sequence",
              "nodes": [
                fake_fetch("uploads1", vec!["file1"]),
                fake_fetch("uploads2", vec!["file2"])
              ]
            })
        );
    }

    #[test]
    fn test_embedded_sequence_into_parallel() {
        let query_plan = fake_query_plan(json!({
          "kind": "Parallel",
          "nodes": [
            fake_fetch("uploads1", vec!["file1"]),
            fake_fetch("subgraph1", vec!["not_a_file"]),
            fake_fetch("uploads2", vec!["file2"])
          ]
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file1".to_owned()],
            "1".to_owned() => vec!["variables.file2".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_eq!(
            to_root_json(result.unwrap()),
            json!({
              "kind": "Parallel",
              "nodes": [
                fake_fetch("subgraph1", vec!["not_a_file"]),
                {
                  "kind": "Sequence",
                  "nodes": [
                    fake_fetch("uploads1", vec!["file1"]),
                    fake_fetch("uploads2", vec!["file2"])
                  ]
                }
              ]
            })
        );
    }

    #[test]
    fn test_fix_order_in_parallel() {
        let query_plan = fake_query_plan(json!({
          "kind": "Parallel",
          "nodes": [
            fake_fetch("uploads1", vec!["file1"]),
            fake_fetch("uploads2", vec!["file0"])
          ]
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file0".to_owned()],
            "1".to_owned() => vec!["variables.file1".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_eq!(
            to_root_json(result.unwrap()),
            json!({
              "kind": "Sequence",
              "nodes": [
                fake_fetch("uploads2", vec!["file0"]),
                fake_fetch("uploads1", vec!["file1"])
              ]
            })
        );
    }

    #[test]
    fn test_parallel_with_overlapping_variables() {
        let query_plan = fake_query_plan(json!({
          "kind": "Parallel",
          "nodes": [
            fake_fetch("uploads1", vec!["files1"]),
            fake_fetch("uploads2", vec!["files2"])
          ]
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.files1.0".to_owned()],
            "1".to_owned() => vec!["variables.files2.0".to_owned()],
            "2".to_owned() => vec!["variables.files1.1".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_matches!(result, Err(FileUploadError::MisorderedVariables));
    }

    #[test]
    fn test_parallel_with_overlapping_fetch_nodes() {
        let query_plan = fake_query_plan(json!({
          "kind": "Parallel",
          "nodes": [
            fake_fetch("uploads1", vec!["file1", "file3"]),
            fake_fetch("uploads2", vec!["file2"])
          ]
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file1".to_owned()],
            "1".to_owned() => vec!["variables.file2".to_owned()],
            "2".to_owned() => vec!["variables.file3".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_matches!(result, Err(FileUploadError::MisorderedVariables));
    }

    #[test]
    fn test_parallel_with_duplicated_variables() {
        let query_plan = fake_query_plan(json!({
          "kind": "Parallel",
          "nodes": [
            fake_fetch("uploads1", vec!["file1"]),
            fake_fetch("uploads2", vec!["file2", "file3"]),
            fake_fetch("uploads3", vec!["file1"]),
            fake_fetch("uploads4", vec!["file2", "file4"])
          ]
        }));

        let map_field = MapField::new(indexmap! {
            "0".to_owned() => vec!["variables.file1".to_owned()],
            "1".to_owned() => vec!["variables.file2".to_owned()],
            "2".to_owned() => vec!["variables.file3".to_owned()],
            "3".to_owned() => vec!["variables.file4".to_owned()],
        })
        .unwrap();

        let result = rearrange_query_plan(&query_plan, &map_field);
        assert_matches!(
            result,
            Err(FileUploadError::DuplicateVariableUsages(ref variables)) if variables == "$file1, $file2",
        );
    }
}
