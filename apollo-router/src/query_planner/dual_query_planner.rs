//! Running two query planner implementations and comparing their results

use std::borrow::Borrow;
use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Instant;

use apollo_compiler::ast;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_federation::query_plan::query_planner::QueryPlanner;
use apollo_federation::query_plan::QueryPlan;

use super::fetch::FetchNode;
use super::fetch::SubgraphOperation;
use super::subscription::SubscriptionNode;
use super::FlattenNode;
use crate::error::format_bridge_errors;
use crate::executable::USING_CATCH_UNWIND;
use crate::query_planner::bridge_query_planner::metric_query_planning_plan_duration;
use crate::query_planner::bridge_query_planner::RUST_QP_MODE;
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
    pub(crate) operation_name: Option<String>,
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
            let name = self
                .operation_name
                .clone()
                .map(Name::try_from)
                .transpose()?;
            USING_CATCH_UNWIND.set(true);

            let start = Instant::now();

            // No question mark operator or macro from here …
            let result = self.rust_planner.build_query_plan(&self.document, name);

            metric_query_planning_plan_duration(RUST_QP_MODE, start);

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
        let operation_desc = if let Ok(operation) = self.document.operations.get(name) {
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
        operation_name: _, // ignored (reordered parallel fetches may have different names)
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
        && vec_matches_sorted(variable_usages, &other.variable_usages)
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
    let this_ast = match ast::Document::parse(this.as_serialized(), "this_operation.graphql") {
        Ok(document) => document,
        Err(_) => {
            // TODO: log error
            return false;
        }
    };
    let other_ast = match ast::Document::parse(other.as_serialized(), "other_operation.graphql") {
        Ok(document) => document,
        Err(_) => {
            // TODO: log error
            return false;
        }
    };
    same_ast_document(&this_ast, &other_ast)
}

// The rest is calling the comparison functions above instead of `PartialEq`,
// but otherwise behave just like `PartialEq`:

// Note: Reexported under `apollo_compiler::_private`
pub fn plan_matches(js_plan: &QueryPlanResult, rust_plan: &QueryPlan) -> bool {
    let js_root_node = &js_plan.query_plan.node;
    let rust_root_node = convert_root_query_plan_node(rust_plan);
    opt_plan_node_matches(js_root_node, &rust_root_node)
}

pub fn diff_plan(js_plan: &QueryPlanResult, rust_plan: &QueryPlan) -> String {
    let js_root_node = &js_plan.query_plan.node;
    let rust_root_node = convert_root_query_plan_node(rust_plan);

    match (js_root_node, rust_root_node) {
        (None, None) => String::from(""),
        (None, Some(rust)) => {
            let rust = &format!("{rust:#?}");
            let differences = diff::lines("", rust);
            render_diff(&differences)
        }
        (Some(js), None) => {
            let js = &format!("{js:#?}");
            let differences = diff::lines(js, "");
            render_diff(&differences)
        }
        (Some(js), Some(rust)) => {
            let rust = &format!("{rust:#?}");
            let js = &format!("{js:#?}");
            let differences = diff::lines(js, rust);
            render_diff(&differences)
        }
    }
}

fn opt_plan_node_matches(
    this: &Option<impl Borrow<PlanNode>>,
    other: &Option<impl Borrow<PlanNode>>,
) -> bool {
    match (this, other) {
        (None, None) => true,
        (None, Some(_)) | (Some(_), None) => false,
        (Some(this), Some(other)) => plan_node_matches(this.borrow(), other.borrow()),
    }
}

fn vec_matches<T>(this: &[T], other: &[T], item_matches: impl Fn(&T, &T) -> bool) -> bool {
    this.len() == other.len()
        && std::iter::zip(this, other).all(|(this, other)| item_matches(this, other))
}

fn vec_matches_sorted<T: Ord + Clone>(this: &[T], other: &[T]) -> bool {
    let mut this_sorted = this.to_owned();
    let mut other_sorted = other.to_owned();
    this_sorted.sort();
    other_sorted.sort();
    vec_matches(&this_sorted, &other_sorted, T::eq)
}

fn vec_matches_sorted_by<T: Eq + Clone>(
    this: &[T],
    other: &[T],
    compare: impl Fn(&T, &T) -> std::cmp::Ordering,
) -> bool {
    let mut this_sorted = this.to_owned();
    let mut other_sorted = other.to_owned();
    this_sorted.sort_by(&compare);
    other_sorted.sort_by(&compare);
    vec_matches(&this_sorted, &other_sorted, T::eq)
}

// performs a set comparison, ignoring order
fn vec_matches_as_set<T>(this: &[T], other: &[T], item_matches: impl Fn(&T, &T) -> bool) -> bool {
    // Set-inclusion test in both directions
    this.len() == other.len()
        && this.iter().all(|this_node| {
            other
                .iter()
                .any(|other_node| item_matches(this_node, other_node))
        })
        && other.iter().all(|other_node| {
            this.iter()
                .any(|this_node| item_matches(this_node, other_node))
        })
}

fn plan_node_matches(this: &PlanNode, other: &PlanNode) -> bool {
    match (this, other) {
        (PlanNode::Sequence { nodes: this }, PlanNode::Sequence { nodes: other }) => {
            vec_matches(this, other, plan_node_matches)
        }
        (PlanNode::Parallel { nodes: this }, PlanNode::Parallel { nodes: other }) => {
            vec_matches_as_set(this, other, plan_node_matches)
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

//==================================================================================================
// AST comparison functions

fn same_ast_document(x: &ast::Document, y: &ast::Document) -> bool {
    fn split_definitions(
        doc: &ast::Document,
    ) -> (
        Vec<&ast::OperationDefinition>,
        Vec<&ast::FragmentDefinition>,
        Vec<&ast::Definition>,
    ) {
        let mut operations: Vec<&ast::OperationDefinition> = Vec::new();
        let mut fragments: Vec<&ast::FragmentDefinition> = Vec::new();
        let mut others: Vec<&ast::Definition> = Vec::new();
        for def in doc.definitions.iter() {
            match def {
                ast::Definition::OperationDefinition(op) => operations.push(op),
                ast::Definition::FragmentDefinition(frag) => fragments.push(frag),
                _ => others.push(def),
            }
        }
        fragments.sort_by_key(|frag| frag.name.clone());
        (operations, fragments, others)
    }

    let (x_ops, x_frags, x_others) = split_definitions(x);
    let (y_ops, y_frags, y_others) = split_definitions(y);

    debug_assert!(x_others.is_empty(), "Unexpected definition types");
    debug_assert!(y_others.is_empty(), "Unexpected definition types");
    debug_assert!(
        x_ops.len() == y_ops.len(),
        "Different number of operation definitions"
    );

    x_ops.len() == y_ops.len()
        && x_ops
            .iter()
            .zip(y_ops.iter())
            .all(|(x_op, y_op)| same_ast_operation_definition(x_op, y_op))
        && x_frags.len() == y_frags.len()
        && x_frags
            .iter()
            .zip(y_frags.iter())
            .all(|(x_frag, y_frag)| same_ast_fragment_definition(x_frag, y_frag))
}

fn same_ast_operation_definition(
    x: &ast::OperationDefinition,
    y: &ast::OperationDefinition,
) -> bool {
    // Note: Operation names are ignored, since parallel fetches may have different names.
    x.operation_type == y.operation_type
        && vec_matches_sorted_by(&x.variables, &y.variables, |x, y| x.name.cmp(&y.name))
        && x.directives == y.directives
        && same_ast_selection_set_sorted(&x.selection_set, &y.selection_set)
}

fn same_ast_fragment_definition(x: &ast::FragmentDefinition, y: &ast::FragmentDefinition) -> bool {
    x.name == y.name
        && x.type_condition == y.type_condition
        && x.directives == y.directives
        && same_ast_selection_set_sorted(&x.selection_set, &y.selection_set)
}

// Copied and modified from `apollo_federation::operation::SelectionKey`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SelectionKey {
    Field {
        /// The field alias (if specified) or field name in the resulting selection set.
        response_name: Name,
        directives: ast::DirectiveList,
    },
    FragmentSpread {
        /// The name of the fragment.
        fragment_name: Name,
        directives: ast::DirectiveList,
    },
    InlineFragment {
        /// The optional type condition of the fragment.
        type_condition: Option<Name>,
        directives: ast::DirectiveList,
    },
}

fn get_selection_key(selection: &ast::Selection) -> SelectionKey {
    match selection {
        ast::Selection::Field(field) => SelectionKey::Field {
            response_name: field.response_name().clone(),
            directives: field.directives.clone(),
        },
        ast::Selection::FragmentSpread(fragment) => SelectionKey::FragmentSpread {
            fragment_name: fragment.fragment_name.clone(),
            directives: fragment.directives.clone(),
        },
        ast::Selection::InlineFragment(fragment) => SelectionKey::InlineFragment {
            type_condition: fragment.type_condition.clone(),
            directives: fragment.directives.clone(),
        },
    }
}

use std::ops::Not;

/// Get the sub-selections of a selection.
fn get_selection_set(selection: &ast::Selection) -> Option<&Vec<ast::Selection>> {
    match selection {
        ast::Selection::Field(field) => field
            .selection_set
            .is_empty()
            .not()
            .then(|| &field.selection_set),
        ast::Selection::FragmentSpread(_) => None,
        ast::Selection::InlineFragment(fragment) => Some(&fragment.selection_set),
    }
}

fn same_ast_selection(x: &ast::Selection, y: &ast::Selection) -> bool {
    let x_key = get_selection_key(x);
    let y_key = get_selection_key(y);
    if x_key != y_key {
        return false;
    }
    let x_selections = get_selection_set(x);
    let y_selections = get_selection_set(y);
    match (x_selections, y_selections) {
        (Some(x), Some(y)) => same_ast_selection_set_sorted(x, y),
        (None, None) => true,
        _ => false,
    }
}

fn hash_value<T: Hash>(x: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    x.hash(&mut hasher);
    hasher.finish()
}

fn hash_selection_key(selection: &ast::Selection) -> u64 {
    hash_value(&get_selection_key(selection))
}

fn same_ast_selection_set_sorted(x: &[ast::Selection], y: &[ast::Selection]) -> bool {
    fn sorted_by_selection_key(s: &[ast::Selection]) -> Vec<&ast::Selection> {
        let mut sorted: Vec<&ast::Selection> = s.iter().collect();
        sorted.sort_by_key(|x| hash_selection_key(x));
        sorted
    }

    if x.len() != y.len() {
        return false;
    }
    sorted_by_selection_key(x)
        .into_iter()
        .zip(sorted_by_selection_key(y))
        .all(|(x, y)| same_ast_selection(x, y))
}

#[cfg(test)]
mod ast_comparison_tests {
    use super::*;

    #[test]
    fn test_query_variable_decl_order() {
        let op_x = r#"query($qv2: String!, $qv1: Int!) { x(arg1: $qv1, arg2: $qv2) }"#;
        let op_y = r#"query($qv1: Int!, $qv2: String!) { x(arg1: $qv1, arg2: $qv2) }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y));
    }

    #[test]
    fn test_entities_selection_order() {
        let op_x = r#"
            query subgraph1__1($representations: [_Any!]!) {
                _entities(representations: $representations) { x { w } y }
            }
            "#;
        let op_y = r#"
            query subgraph1__1($representations: [_Any!]!) {
                _entities(representations: $representations) { y x { w } }
            }
            "#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y));
    }

    #[test]
    fn test_top_level_selection_order() {
        let op_x = r#"{ x { w z } y }"#;
        let op_y = r#"{ y x { z w } }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y));
    }

    #[test]
    fn test_fragment_definition_order() {
        let op_x = r#"{ q { ...f1 ...f2 } } fragment f1 on T { x y } fragment f2 on T { w z }"#;
        let op_y = r#"{ q { ...f1 ...f2 } } fragment f2 on T { w z } fragment f1 on T { x y }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y));
    }
}
