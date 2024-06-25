//! Running two query planner implementations and comparing their results

use std::borrow::Borrow;
use std::sync::Arc;
use std::sync::OnceLock;

use apollo_compiler::ast::Name;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::NodeStr;
use apollo_federation::query_plan::query_planner::QueryPlanner;

use super::fetch::FetchNode;
use super::fetch::SubgraphOperation;
use super::subscription::SubscriptionNode;
use super::FlattenNode;
use crate::error::format_bridge_errors;
use crate::executable::USING_CATCH_UNWIND;
use crate::query_planner::convert::convert_root_query_plan_node;
use crate::query_planner::render_diff;
use crate::query_planner::DeferredNode;
use crate::query_planner::PlanNode;
use crate::query_planner::Primary;
use crate::query_planner::QueryPlanResult;

/// Jobs are dropped if this many are already queued
const QUEUE_SIZE: usize = 10;
const WORKER_THREAD_COUNT: usize = 1;

pub(crate) struct BothModeComparisonJob {
    pub(crate) rust_planner: Arc<QueryPlanner>,
    pub(crate) document: Arc<Valid<ExecutableDocument>>,
    pub(crate) operation_name: Option<NodeStr>,
    pub(crate) js_result: Result<QueryPlanResult, Arc<Vec<router_bridge::planner::PlanError>>>,
}

type Queue = crossbeam_channel::Sender<BothModeComparisonJob>;

static QUEUE: OnceLock<Queue> = OnceLock::new();

fn queue() -> &'static Queue {
    QUEUE.get_or_init(|| {
        let (sender, receiver) = crossbeam_channel::bounded::<BothModeComparisonJob>(QUEUE_SIZE);
        for _ in 0..WORKER_THREAD_COUNT {
            let job_receiver = receiver.clone();
            std::thread::spawn(move || {
                for job in job_receiver {
                    job.execute()
                }
            });
        }
        sender
    })
}

impl BothModeComparisonJob {
    pub(crate) fn schedule(self) {
        // We use a bounded queue: try_send returns an error when full. This is fine.
        // We prefer dropping some comparison jobs and only gathering some of the data
        // rather than consume too much resources.
        //
        // Either way we move on and let this thread continue proceed with the query plan from JS.
        let _ = queue().try_send(self).is_err();
    }

    fn execute(self) {
        // TODO: once the Rust query planner does not use `todo!()` anymore,
        // remove `USING_CATCH_UNWIND` and this use of `catch_unwind`.
        let rust_result = std::panic::catch_unwind(|| {
            let name = self.operation_name.clone().map(Name::new).transpose()?;
            USING_CATCH_UNWIND.set(true);
            // No question mark operator or macro from here …
            let result = self.rust_planner.build_query_plan(&self.document, name);
            // … to here, so the thread can only eiher reach here or panic.
            // We unset USING_CATCH_UNWIND in both cases.
            USING_CATCH_UNWIND.set(false);
            result
        })
        .unwrap_or_else(|panic| {
            USING_CATCH_UNWIND.set(false);
            Err(apollo_federation::error::FederationError::internal(
                format!(
                    "query planner panicked: {}",
                    panic
                        .downcast_ref::<String>()
                        .map(|s| s.as_str())
                        .or_else(|| panic.downcast_ref::<&str>().copied())
                        .unwrap_or_default()
                ),
            ))
        });

        let name = self.operation_name.as_deref();
        let operation_desc = if let Ok(operation) = self.document.get_operation(name) {
            if let Some(parsed_name) = &operation.name {
                format!(" in {} `{parsed_name}`", operation.operation_type)
            } else {
                format!(" in anonymous {}", operation.operation_type)
            }
        } else {
            String::new()
        };

        let is_matched;
        match (&self.js_result, &rust_result) {
            (Err(js_errors), Ok(_)) => {
                tracing::warn!(
                    "JS query planner error{operation_desc}: {}",
                    format_bridge_errors(js_errors)
                );
                is_matched = false;
            }
            (Ok(_), Err(rust_error)) => {
                tracing::warn!("Rust query planner error{operation_desc}: {}", rust_error);
                is_matched = false;
            }
            (Err(_), Err(_)) => {
                is_matched = true;
            }

            (Ok(js_plan), Ok(rust_plan)) => {
                let js_root_node = &js_plan.query_plan.node;
                let rust_root_node = convert_root_query_plan_node(rust_plan);
                is_matched = opt_plan_node_matches(js_root_node, &rust_root_node);
                if is_matched {
                    tracing::debug!("JS and Rust query plans match{operation_desc}! 🎉");
                } else {
                    tracing::debug!("JS v.s. Rust query plan mismatch{operation_desc}");
                    if let Some(formatted) = &js_plan.formatted_query_plan {
                        tracing::debug!(
                            "Diff of formatted plans:\n{}",
                            render_diff(&diff::lines(formatted, &rust_plan.to_string()))
                        );
                    }
                    tracing::trace!("JS query plan Debug: {js_root_node:#?}");
                    tracing::trace!("Rust query plan Debug: {rust_root_node:#?}");
                }
            }
        }

        u64_counter!(
            "apollo.router.operations.query_planner.both",
            "Comparing JS v.s. Rust query plans",
            1,
            "generation.is_matched" = is_matched,
            "generation.js_error" = self.js_result.is_err(),
            "generation.rust_error" = rust_result.is_err()
        );
    }
}

// Specific comparison functions

fn fetch_node_matches(this: &FetchNode, other: &FetchNode) -> bool {
    let FetchNode {
        service_name,
        requires,
        variable_usages,
        operation,
        operation_name,
        operation_kind,
        id,
        input_rewrites,
        output_rewrites,
        context_rewrites,
        schema_aware_hash: _, // ignored
        authorization,
    } = this;
    *service_name == other.service_name
        && *requires == other.requires
        && *variable_usages == other.variable_usages
        && *operation_name == other.operation_name
        && *operation_kind == other.operation_kind
        && *id == other.id
        && *input_rewrites == other.input_rewrites
        && *output_rewrites == other.output_rewrites
        && *context_rewrites == other.context_rewrites
        && *authorization == other.authorization
        && operation_matches(operation, &other.operation)
}

fn subscription_primary_matches(this: &SubscriptionNode, other: &SubscriptionNode) -> bool {
    let SubscriptionNode {
        service_name,
        variable_usages,
        operation,
        operation_name,
        operation_kind,
        input_rewrites,
        output_rewrites,
    } = this;
    *service_name == other.service_name
        && *variable_usages == other.variable_usages
        && *operation_name == other.operation_name
        && *operation_kind == other.operation_kind
        && *input_rewrites == other.input_rewrites
        && *output_rewrites == other.output_rewrites
        && operation_matches(operation, &other.operation)
}

fn operation_matches(this: &SubgraphOperation, other: &SubgraphOperation) -> bool {
    operation_without_whitespace(this) == operation_without_whitespace(other)
}

fn operation_without_whitespace(op: &SubgraphOperation) -> String {
    op.as_serialized().replace([' ', '\n'], "")
}

// The rest is calling the comparison functions above instead of `PartialEq`,
// but otherwise behave just like `PartialEq`:

/// Reexported under `apollo_compiler::_private`
pub fn opt_plan_node_matches(
    this: &Option<impl Borrow<PlanNode>>,
    other: &Option<impl Borrow<PlanNode>>,
) -> bool {
    match (this, other) {
        (None, None) => true,
        (None, Some(_)) | (Some(_), None) => false,
        (Some(this), Some(other)) => plan_node_matches(this.borrow(), other.borrow()),
    }
}

fn vec_matches<T>(this: &Vec<T>, other: &Vec<T>, item_matches: impl Fn(&T, &T) -> bool) -> bool {
    this.len() == other.len()
        && std::iter::zip(this, other).all(|(this, other)| item_matches(this, other))
}

fn plan_node_matches(this: &PlanNode, other: &PlanNode) -> bool {
    match (this, other) {
        (PlanNode::Sequence { nodes: this }, PlanNode::Sequence { nodes: other })
        | (PlanNode::Parallel { nodes: this }, PlanNode::Parallel { nodes: other }) => {
            vec_matches(this, other, plan_node_matches)
        }
        (PlanNode::Fetch(this), PlanNode::Fetch(other)) => fetch_node_matches(this, other),
        (PlanNode::Flatten(this), PlanNode::Flatten(other)) => flatten_node_matches(this, other),
        (
            PlanNode::Defer { primary, deferred },
            PlanNode::Defer {
                primary: other_primary,
                deferred: other_deferred,
            },
        ) => {
            defer_primary_node_matches(primary, other_primary)
                && vec_matches(deferred, other_deferred, deferred_node_matches)
        }
        (
            PlanNode::Subscription { primary, rest },
            PlanNode::Subscription {
                primary: other_primary,
                rest: other_rest,
            },
        ) => {
            subscription_primary_matches(primary, other_primary)
                && opt_plan_node_matches(rest, other_rest)
        }
        (
            PlanNode::Condition {
                condition,
                if_clause,
                else_clause,
            },
            PlanNode::Condition {
                condition: other_condition,
                if_clause: other_if_clause,
                else_clause: other_else_clause,
            },
        ) => {
            condition == other_condition
                && opt_plan_node_matches(if_clause, other_if_clause)
                && opt_plan_node_matches(else_clause, other_else_clause)
        }
        _ => false,
    }
}

fn defer_primary_node_matches(this: &Primary, other: &Primary) -> bool {
    let Primary { subselection, node } = this;
    *subselection == other.subselection && opt_plan_node_matches(node, &other.node)
}

fn deferred_node_matches(this: &DeferredNode, other: &DeferredNode) -> bool {
    let DeferredNode {
        depends,
        label,
        query_path,
        subselection,
        node,
    } = this;
    *depends == other.depends
        && *label == other.label
        && *query_path == other.query_path
        && *subselection == other.subselection
        && opt_plan_node_matches(node, &other.node)
}

fn flatten_node_matches(this: &FlattenNode, other: &FlattenNode) -> bool {
    let FlattenNode { path, node } = this;
    *path == other.path && plan_node_matches(node, &other.node)
}
