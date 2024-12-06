// Semantic comparison of JS and Rust query plans

use std::borrow::Borrow;
use std::collections::hash_map::HashMap;
use std::fmt::Write;
use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;

use apollo_compiler::ast;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_federation::query_plan::QueryPlan as NativeQueryPlan;

use super::convert::convert_root_query_plan_node;
use super::fetch::FetchNode;
use super::fetch::SubgraphOperation;
use super::rewrites::DataRewrite;
use super::selection::Selection;
use super::subscription::SubscriptionNode;
use super::DeferredNode;
use super::FlattenNode;
use super::PlanNode;
use super::Primary;
use super::QueryPlanResult;

//==================================================================================================
// Public interface

pub struct MatchFailure {
    description: String,
    backtrace: std::backtrace::Backtrace,
}

impl MatchFailure {
    pub fn description(&self) -> String {
        self.description.clone()
    }

    pub fn full_description(&self) -> String {
        format!("{}\n\nBacktrace:\n{}", self.description, self.backtrace)
    }

    fn new(description: String) -> MatchFailure {
        MatchFailure {
            description,
            backtrace: std::backtrace::Backtrace::force_capture(),
        }
    }

    fn add_description(self: MatchFailure, description: &str) -> MatchFailure {
        MatchFailure {
            description: format!("{}\n{}", self.description, description),
            backtrace: self.backtrace,
        }
    }
}

macro_rules! check_match {
    ($pred:expr) => {
        if !$pred {
            return Err(MatchFailure::new(format!(
                "mismatch at {}",
                stringify!($pred)
            )));
        }
    };
}

macro_rules! check_match_eq {
    ($a:expr, $b:expr) => {
        if $a != $b {
            let message = format!(
                "mismatch between {} and {}:\nleft: {:?}\nright: {:?}",
                stringify!($a),
                stringify!($b),
                $a,
                $b
            );
            return Err(MatchFailure::new(message));
        }
    };
}

// Note: Reexported under `apollo_router::_private`
pub fn plan_matches(
    js_plan: &QueryPlanResult,
    rust_plan: &NativeQueryPlan,
) -> Result<(), MatchFailure> {
    let js_root_node = &js_plan.query_plan.node;
    let rust_root_node = convert_root_query_plan_node(rust_plan);
    opt_plan_node_matches(js_root_node, &rust_root_node)
}

// Note: Reexported under `apollo_router::_private`
pub fn diff_plan(js_plan: &QueryPlanResult, rust_plan: &NativeQueryPlan) -> String {
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

// Note: Reexported under `apollo_router::_private`
pub fn render_diff(differences: &[diff::Result<&str>]) -> String {
    let mut output = String::new();
    for diff_line in differences {
        match diff_line {
            diff::Result::Left(l) => {
                let trimmed = l.trim();
                if !trimmed.starts_with('#') && !trimmed.is_empty() {
                    writeln!(&mut output, "-{l}").expect("write will never fail");
                } else {
                    writeln!(&mut output, " {l}").expect("write will never fail");
                }
            }
            diff::Result::Both(l, _) => {
                writeln!(&mut output, " {l}").expect("write will never fail");
            }
            diff::Result::Right(r) => {
                let trimmed = r.trim();
                if trimmed != "---" && !trimmed.is_empty() {
                    writeln!(&mut output, "+{r}").expect("write will never fail");
                }
            }
        }
    }
    output
}

//==================================================================================================
// Vec comparison functions

fn vec_matches<T>(this: &[T], other: &[T], item_matches: impl Fn(&T, &T) -> bool) -> bool {
    this.len() == other.len()
        && std::iter::zip(this, other).all(|(this, other)| item_matches(this, other))
}

fn vec_matches_result<T>(
    this: &[T],
    other: &[T],
    item_matches: impl Fn(&T, &T) -> Result<(), MatchFailure>,
) -> Result<(), MatchFailure> {
    check_match_eq!(this.len(), other.len());
    std::iter::zip(this, other)
        .enumerate()
        .try_fold((), |_acc, (index, (this, other))| {
            item_matches(this, other)
                .map_err(|err| err.add_description(&format!("under item[{}]", index)))
        })?;
    Ok(())
}

fn vec_matches_sorted<T: Ord + Clone>(this: &[T], other: &[T]) -> bool {
    let mut this_sorted = this.to_owned();
    let mut other_sorted = other.to_owned();
    this_sorted.sort();
    other_sorted.sort();
    vec_matches(&this_sorted, &other_sorted, T::eq)
}

fn vec_matches_sorted_by<T: Clone>(
    this: &[T],
    other: &[T],
    compare: impl Fn(&T, &T) -> std::cmp::Ordering,
    item_matches: impl Fn(&T, &T) -> bool,
) -> bool {
    let mut this_sorted = this.to_owned();
    let mut other_sorted = other.to_owned();
    this_sorted.sort_by(&compare);
    other_sorted.sort_by(&compare);
    vec_matches(&this_sorted, &other_sorted, item_matches)
}

fn vec_matches_result_sorted_by<T: Clone>(
    this: &[T],
    other: &[T],
    compare: impl Fn(&T, &T) -> std::cmp::Ordering,
    item_matches: impl Fn(&T, &T) -> Result<(), MatchFailure>,
) -> Result<(), MatchFailure> {
    check_match_eq!(this.len(), other.len());
    let mut this_sorted = this.to_owned();
    let mut other_sorted = other.to_owned();
    this_sorted.sort_by(&compare);
    other_sorted.sort_by(&compare);
    std::iter::zip(&this_sorted, &other_sorted)
        .try_fold((), |_acc, (this, other)| item_matches(this, other))?;
    Ok(())
}

// `this` vector includes `other` vector as a set
fn vec_includes_as_set<T>(this: &[T], other: &[T], item_matches: impl Fn(&T, &T) -> bool) -> bool {
    other.iter().all(|other_node| {
        this.iter()
            .any(|this_node| item_matches(this_node, other_node))
    })
}

// performs a set comparison, ignoring order
fn vec_matches_as_set<T>(this: &[T], other: &[T], item_matches: impl Fn(&T, &T) -> bool) -> bool {
    // Set-inclusion test in both directions
    this.len() == other.len()
        && vec_includes_as_set(this, other, &item_matches)
        && vec_includes_as_set(other, this, &item_matches)
}

// Forward/reverse mappings from one Vec items (indices) to another.
type VecMapping = (HashMap<usize, usize>, HashMap<usize, usize>);

// performs a set comparison, ignoring order
// and returns a mapping from `this` to `other`.
fn vec_matches_as_set_with_mapping<T>(
    this: &[T],
    other: &[T],
    item_matches: impl Fn(&T, &T) -> bool,
) -> VecMapping {
    // Set-inclusion test in both directions
    // - record forward/reverse mapping from this items <-> other items for reporting mismatches
    let mut forward_map: HashMap<usize, usize> = HashMap::new();
    let mut reverse_map: HashMap<usize, usize> = HashMap::new();
    for (this_pos, this_node) in this.iter().enumerate() {
        if let Some(other_pos) = other
            .iter()
            .position(|other_node| item_matches(this_node, other_node))
        {
            forward_map.insert(this_pos, other_pos);
            reverse_map.insert(other_pos, this_pos);
        }
    }
    for (other_pos, other_node) in other.iter().enumerate() {
        if reverse_map.contains_key(&other_pos) {
            continue;
        }
        if let Some(this_pos) = this
            .iter()
            .position(|this_node| item_matches(this_node, other_node))
        {
            forward_map.insert(this_pos, other_pos);
            reverse_map.insert(other_pos, this_pos);
        }
    }
    (forward_map, reverse_map)
}

// Returns a formatted mismatch message and an optional pair of mismatched positions if the pair
// are the only remaining unmatched items.
fn format_mismatch_as_set(
    this_len: usize,
    other_len: usize,
    forward_map: &HashMap<usize, usize>,
    reverse_map: &HashMap<usize, usize>,
) -> Result<(String, Option<(usize, usize)>), std::fmt::Error> {
    let mut ret = String::new();
    let buf = &mut ret;
    write!(buf, "- mapping from left to right: [")?;
    let mut this_missing_pos = None;
    for this_pos in 0..this_len {
        if this_pos != 0 {
            write!(buf, ", ")?;
        }
        if let Some(other_pos) = forward_map.get(&this_pos) {
            write!(buf, "{}", other_pos)?;
        } else {
            this_missing_pos = Some(this_pos);
            write!(buf, "?")?;
        }
    }
    writeln!(buf, "]")?;

    write!(buf, "- left-over on the right: [")?;
    let mut other_missing_count = 0;
    let mut other_missing_pos = None;
    for other_pos in 0..other_len {
        if reverse_map.get(&other_pos).is_none() {
            if other_missing_count != 0 {
                write!(buf, ", ")?;
            }
            other_missing_count += 1;
            other_missing_pos = Some(other_pos);
            write!(buf, "{}", other_pos)?;
        }
    }
    write!(buf, "]")?;
    let unmatched_pair = if let (Some(this_missing_pos), Some(other_missing_pos)) =
        (this_missing_pos, other_missing_pos)
    {
        if this_len == 1 + forward_map.len() && other_len == 1 + reverse_map.len() {
            // Special case: There are only one missing item on each side. They are supposed to
            // match each other.
            Some((this_missing_pos, other_missing_pos))
        } else {
            None
        }
    } else {
        None
    };
    Ok((ret, unmatched_pair))
}

fn vec_matches_result_as_set<T>(
    this: &[T],
    other: &[T],
    item_matches: impl Fn(&T, &T) -> Result<(), MatchFailure>,
) -> Result<VecMapping, MatchFailure> {
    // Set-inclusion test in both directions
    // - record forward/reverse mapping from this items <-> other items for reporting mismatches
    let (forward_map, reverse_map) =
        vec_matches_as_set_with_mapping(this, other, |a, b| item_matches(a, b).is_ok());
    if forward_map.len() == this.len() && reverse_map.len() == other.len() {
        Ok((forward_map, reverse_map))
    } else {
        // report mismatch
        let Ok((message, unmatched_pair)) =
            format_mismatch_as_set(this.len(), other.len(), &forward_map, &reverse_map)
        else {
            // Exception: Unable to format mismatch report => fallback to most generic message
            return Err(MatchFailure::new(
                "mismatch at vec_matches_result_as_set (failed to format mismatched sets)"
                    .to_string(),
            ));
        };
        if let Some(unmatched_pair) = unmatched_pair {
            // found a unique pair to report => use that pair's error message
            let Err(err) = item_matches(&this[unmatched_pair.0], &other[unmatched_pair.1]) else {
                // Exception: Unable to format unique pair mismatch error => fallback to overall report
                return Err(MatchFailure::new(format!(
                    "mismatched sets (failed to format unique pair mismatch error):\n{}",
                    message
                )));
            };
            Err(err.add_description(&format!(
                "under a sole unmatched pair ({} -> {}) in a set comparison",
                unmatched_pair.0, unmatched_pair.1
            )))
        } else {
            Err(MatchFailure::new(format!("mismatched sets:\n{}", message)))
        }
    }
}

//==================================================================================================
// PlanNode comparison functions

fn option_to_string(name: Option<impl ToString>) -> String {
    name.map_or_else(|| "<none>".to_string(), |name| name.to_string())
}

fn plan_node_matches(this: &PlanNode, other: &PlanNode) -> Result<(), MatchFailure> {
    match (this, other) {
        (PlanNode::Sequence { nodes: this }, PlanNode::Sequence { nodes: other }) => {
            vec_matches_result(this, other, plan_node_matches)
                .map_err(|err| err.add_description("under Sequence node"))?;
        }
        (PlanNode::Parallel { nodes: this }, PlanNode::Parallel { nodes: other }) => {
            vec_matches_result_as_set(this, other, plan_node_matches)
                .map_err(|err| err.add_description("under Parallel node"))?;
        }
        (PlanNode::Fetch(this), PlanNode::Fetch(other)) => {
            fetch_node_matches(this, other).map_err(|err| {
                err.add_description(&format!(
                    "under Fetch node (operation name: {})",
                    option_to_string(this.operation_name.as_ref())
                ))
            })?;
        }
        (PlanNode::Flatten(this), PlanNode::Flatten(other)) => {
            flatten_node_matches(this, other).map_err(|err| {
                err.add_description(&format!("under Flatten node (path: {})", this.path))
            })?;
        }
        (
            PlanNode::Defer { primary, deferred },
            PlanNode::Defer {
                primary: other_primary,
                deferred: other_deferred,
            },
        ) => {
            defer_primary_node_matches(primary, other_primary)?;
            vec_matches_result(deferred, other_deferred, deferred_node_matches)?;
        }
        (
            PlanNode::Subscription { primary, rest },
            PlanNode::Subscription {
                primary: other_primary,
                rest: other_rest,
            },
        ) => {
            subscription_primary_matches(primary, other_primary)?;
            opt_plan_node_matches(rest, other_rest)
                .map_err(|err| err.add_description("under Subscription"))?;
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
            check_match_eq!(condition, other_condition);
            opt_plan_node_matches(if_clause, other_if_clause)
                .map_err(|err| err.add_description("under Condition node (if_clause)"))?;
            opt_plan_node_matches(else_clause, other_else_clause)
                .map_err(|err| err.add_description("under Condition node (else_clause)"))?;
        }
        _ => {
            return Err(MatchFailure::new(format!(
                "mismatched plan node types\nleft: {:?}\nright: {:?}",
                this, other
            )))
        }
    };
    Ok(())
}

pub(crate) fn opt_plan_node_matches(
    this: &Option<impl Borrow<PlanNode>>,
    other: &Option<impl Borrow<PlanNode>>,
) -> Result<(), MatchFailure> {
    match (this, other) {
        (None, None) => Ok(()),
        (None, Some(_)) | (Some(_), None) => Err(MatchFailure::new(format!(
            "mismatch at opt_plan_node_matches\nleft: {:?}\nright: {:?}",
            this.is_some(),
            other.is_some()
        ))),
        (Some(this), Some(other)) => plan_node_matches(this.borrow(), other.borrow()),
    }
}

fn fetch_node_matches(this: &FetchNode, other: &FetchNode) -> Result<(), MatchFailure> {
    let FetchNode {
        service_name,
        requires,
        variable_usages,
        operation,
        // ignored:
        // reordered parallel fetches may have different names
        operation_name: _,
        operation_kind,
        id,
        input_rewrites,
        output_rewrites,
        context_rewrites,
        // ignored
        schema_aware_hash: _,
        // ignored:
        // when running in comparison mode, the rust plan node does not have
        // the attached cache key metadata for authorisation, since the rust plan is
        // not going to be the one being executed.
        authorization: _,
    } = this;

    check_match_eq!(*service_name, other.service_name);
    check_match_eq!(*operation_kind, other.operation_kind);
    check_match_eq!(*id, other.id);
    check_match!(same_requires(requires, &other.requires));
    check_match!(vec_matches_sorted(variable_usages, &other.variable_usages));
    check_match!(same_rewrites(input_rewrites, &other.input_rewrites));
    check_match!(same_rewrites(output_rewrites, &other.output_rewrites));
    check_match!(same_rewrites(context_rewrites, &other.context_rewrites));
    operation_matches(operation, &other.operation)?;
    Ok(())
}

fn subscription_primary_matches(
    this: &SubscriptionNode,
    other: &SubscriptionNode,
) -> Result<(), MatchFailure> {
    let SubscriptionNode {
        service_name,
        variable_usages,
        operation,
        operation_name: _, // ignored (reordered parallel fetches may have different names)
        operation_kind,
        input_rewrites,
        output_rewrites,
    } = this;
    check_match_eq!(*service_name, other.service_name);
    check_match_eq!(*operation_kind, other.operation_kind);
    check_match!(vec_matches_sorted(variable_usages, &other.variable_usages));
    check_match!(same_rewrites(input_rewrites, &other.input_rewrites));
    check_match!(same_rewrites(output_rewrites, &other.output_rewrites));
    operation_matches(operation, &other.operation)?;
    Ok(())
}

fn defer_primary_node_matches(this: &Primary, other: &Primary) -> Result<(), MatchFailure> {
    let Primary { subselection, node } = this;
    opt_document_string_matches(subselection, &other.subselection)
        .map_err(|err| err.add_description("under defer primary subselection"))?;
    opt_plan_node_matches(node, &other.node)
        .map_err(|err| err.add_description("under defer primary plan node"))
}

fn deferred_node_matches(this: &DeferredNode, other: &DeferredNode) -> Result<(), MatchFailure> {
    let DeferredNode {
        depends,
        label,
        query_path,
        subselection,
        node,
    } = this;

    check_match_eq!(*depends, other.depends);
    check_match_eq!(*label, other.label);
    check_match_eq!(*query_path, other.query_path);
    opt_document_string_matches(subselection, &other.subselection)
        .map_err(|err| err.add_description("under deferred subselection"))?;
    opt_plan_node_matches(node, &other.node)
        .map_err(|err| err.add_description("under deferred node"))
}

fn flatten_node_matches(this: &FlattenNode, other: &FlattenNode) -> Result<(), MatchFailure> {
    let FlattenNode { path, node } = this;
    check_match_eq!(*path, other.path);
    plan_node_matches(node, &other.node)
}

// Copied and modified from `apollo_federation::operation::SelectionKey`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SelectionKey {
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

fn get_selection_key(selection: &Selection) -> SelectionKey {
    match selection {
        Selection::Field(field) => SelectionKey::Field {
            response_name: field.response_name().clone(),
            directives: Default::default(),
        },
        Selection::InlineFragment(fragment) => SelectionKey::InlineFragment {
            type_condition: fragment.type_condition.clone(),
            directives: Default::default(),
        },
    }
}

fn hash_value<T: Hash>(x: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    x.hash(&mut hasher);
    hasher.finish()
}

fn hash_selection_key(selection: &Selection) -> u64 {
    hash_value(&get_selection_key(selection))
}

// Note: This `Selection` struct is a limited version used for the `requires` field.
fn same_selection(x: &Selection, y: &Selection) -> bool {
    match (x, y) {
        (Selection::Field(x), Selection::Field(y)) => {
            x.name == y.name
                && x.alias == y.alias
                && match (&x.selections, &y.selections) {
                    (Some(x), Some(y)) => same_selection_set_sorted(x, y),
                    (None, None) => true,
                    _ => false,
                }
        }
        (Selection::InlineFragment(x), Selection::InlineFragment(y)) => {
            x.type_condition == y.type_condition
                && same_selection_set_sorted(&x.selections, &y.selections)
        }
        _ => false,
    }
}

fn same_selection_set_sorted(x: &[Selection], y: &[Selection]) -> bool {
    fn sorted_by_selection_key(s: &[Selection]) -> Vec<&Selection> {
        let mut sorted: Vec<&Selection> = s.iter().collect();
        sorted.sort_by_key(|x| hash_selection_key(x));
        sorted
    }

    if x.len() != y.len() {
        return false;
    }
    sorted_by_selection_key(x)
        .into_iter()
        .zip(sorted_by_selection_key(y))
        .all(|(x, y)| same_selection(x, y))
}

fn same_requires(x: &[Selection], y: &[Selection]) -> bool {
    vec_matches_as_set(x, y, same_selection)
}

fn same_rewrites(x: &Option<Vec<DataRewrite>>, y: &Option<Vec<DataRewrite>>) -> bool {
    match (x, y) {
        (None, None) => true,
        (Some(x), Some(y)) => vec_matches_as_set(x, y, |a, b| a == b),
        _ => false,
    }
}

fn operation_matches(
    this: &SubgraphOperation,
    other: &SubgraphOperation,
) -> Result<(), MatchFailure> {
    document_str_matches(this.as_serialized(), other.as_serialized())
}

// Compare operation document strings such as query or just selection set.
fn document_str_matches(this: &str, other: &str) -> Result<(), MatchFailure> {
    let this_ast = match ast::Document::parse(this, "this_operation.graphql") {
        Ok(document) => document,
        Err(_) => {
            return Err(MatchFailure::new(
                "Failed to parse this operation".to_string(),
            ));
        }
    };
    let other_ast = match ast::Document::parse(other, "other_operation.graphql") {
        Ok(document) => document,
        Err(_) => {
            return Err(MatchFailure::new(
                "Failed to parse other operation".to_string(),
            ));
        }
    };
    same_ast_document(&this_ast, &other_ast)
}

fn opt_document_string_matches(
    this: &Option<String>,
    other: &Option<String>,
) -> Result<(), MatchFailure> {
    match (this, other) {
        (None, None) => Ok(()),
        (Some(this_sel), Some(other_sel)) => document_str_matches(this_sel, other_sel),
        _ => Err(MatchFailure::new(format!(
            "mismatched at opt_document_string_matches\nleft: {:?}\nright: {:?}",
            this, other
        ))),
    }
}

//==================================================================================================
// AST comparison functions

fn same_ast_document(x: &ast::Document, y: &ast::Document) -> Result<(), MatchFailure> {
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

    check_match_eq!(x_frags.len(), y_frags.len());
    let mut fragment_map: HashMap<Name, Name> = HashMap::new();
    // Assumption: x_frags and y_frags are topologically sorted.
    //             Thus, we can build the fragment name mapping in a single pass and compare
    //             fragment definitions using the mapping at the same time, since earlier fragments
    //             will never reference later fragments.
    x_frags.iter().try_fold((), |_, x_frag| {
        let y_frag = y_frags
            .iter()
            .find(|y_frag| same_ast_fragment_definition(x_frag, y_frag, &fragment_map).is_ok());
        if let Some(y_frag) = y_frag {
            if x_frag.name != y_frag.name {
                // record it only if they are not identical
                fragment_map.insert(x_frag.name.clone(), y_frag.name.clone());
            }
            Ok(())
        } else {
            Err(MatchFailure::new(format!(
                "mismatch: no matching fragment definition for {}",
                x_frag.name
            )))
        }
    })?;

    check_match_eq!(x_ops.len(), y_ops.len());
    x_ops
        .iter()
        .zip(y_ops.iter())
        .try_fold((), |_, (x_op, y_op)| {
            same_ast_operation_definition(x_op, y_op, &fragment_map)
                .map_err(|err| err.add_description("under operation definition"))
        })?;
    Ok(())
}

fn same_ast_operation_definition(
    x: &ast::OperationDefinition,
    y: &ast::OperationDefinition,
    fragment_map: &HashMap<Name, Name>,
) -> Result<(), MatchFailure> {
    // Note: Operation names are ignored, since parallel fetches may have different names.
    check_match_eq!(x.operation_type, y.operation_type);
    vec_matches_result_sorted_by(
        &x.variables,
        &y.variables,
        |a, b| a.name.cmp(&b.name),
        |a, b| same_variable_definition(a, b),
    )
    .map_err(|err| err.add_description("under Variable definition"))?;
    check_match_eq!(x.directives, y.directives);
    check_match!(same_ast_selection_set_sorted(
        &x.selection_set,
        &y.selection_set,
        fragment_map,
    ));
    Ok(())
}

// `x` may be coerced to `y`.
// - `x` should be a value from JS QP.
// - `y` should be a value from Rust QP.
// - Assume: x and y are already checked not equal.
// Due to coercion differences, we need to compare AST values with special cases.
fn ast_value_maybe_coerced_to(x: &ast::Value, y: &ast::Value) -> bool {
    match (x, y) {
        // Special case 1: JS QP may convert an enum value into string.
        // - In this case, compare them as strings.
        (ast::Value::String(ref x), ast::Value::Enum(ref y)) => {
            if x == y.as_str() {
                return true;
            }
        }

        // Special case 2: Rust QP expands a object value by filling in its
        // default field values.
        // - If the Rust QP object value subsumes the JS QP object value, consider it a match.
        // - Assuming the Rust QP object value has only default field values.
        // - Warning: This is an unsound heuristic.
        (ast::Value::Object(ref x), ast::Value::Object(ref y)) => {
            if vec_includes_as_set(y, x, |(yy_name, yy_val), (xx_name, xx_val)| {
                xx_name == yy_name
                    && (xx_val == yy_val || ast_value_maybe_coerced_to(xx_val, yy_val))
            }) {
                return true;
            }
        }

        // Special case 3: JS QP may convert string to int for custom scalars, while Rust doesn't.
        // - Note: This conversion seems a bit difficult to implement in the `apollo-federation`'s
        //         `coerce_value` function, since IntValue's constructor is private to the crate.
        (ast::Value::Int(ref x), ast::Value::String(ref y)) => {
            if x.as_str() == y {
                return true;
            }
        }

        // Recurse into list items.
        (ast::Value::List(ref x), ast::Value::List(ref y)) => {
            if vec_matches(x, y, |xx, yy| {
                xx == yy || ast_value_maybe_coerced_to(xx, yy)
            }) {
                return true;
            }
        }

        _ => {} // otherwise, fall through
    }
    false
}

// Use this function, instead of `VariableDefinition`'s `PartialEq` implementation,
// due to known differences.
fn same_variable_definition(
    x: &ast::VariableDefinition,
    y: &ast::VariableDefinition,
) -> Result<(), MatchFailure> {
    check_match_eq!(x.name, y.name);
    check_match_eq!(x.ty, y.ty);
    if x.default_value != y.default_value {
        if let (Some(x), Some(y)) = (&x.default_value, &y.default_value) {
            if ast_value_maybe_coerced_to(x, y) {
                return Ok(());
            }
        }

        return Err(MatchFailure::new(format!(
            "mismatch between default values:\nleft: {:?}\nright: {:?}",
            x.default_value, y.default_value
        )));
    }
    check_match_eq!(x.directives, y.directives);
    Ok(())
}

fn same_ast_fragment_definition(
    x: &ast::FragmentDefinition,
    y: &ast::FragmentDefinition,
    fragment_map: &HashMap<Name, Name>,
) -> Result<(), MatchFailure> {
    // Note: Fragment names at definitions are ignored.
    check_match_eq!(x.type_condition, y.type_condition);
    check_match_eq!(x.directives, y.directives);
    check_match!(same_ast_selection_set_sorted(
        &x.selection_set,
        &y.selection_set,
        fragment_map,
    ));
    Ok(())
}

fn same_ast_argument_value(x: &ast::Value, y: &ast::Value) -> bool {
    x == y || ast_value_maybe_coerced_to(x, y)
}

fn same_ast_argument(x: &ast::Argument, y: &ast::Argument) -> bool {
    x.name == y.name && same_ast_argument_value(&x.value, &y.value)
}

fn same_ast_arguments(x: &[Node<ast::Argument>], y: &[Node<ast::Argument>]) -> bool {
    vec_matches_sorted_by(
        x,
        y,
        |a, b| a.name.cmp(&b.name),
        |a, b| same_ast_argument(a, b),
    )
}

fn same_directives(x: &ast::DirectiveList, y: &ast::DirectiveList) -> bool {
    vec_matches_sorted_by(
        x,
        y,
        |a, b| a.name.cmp(&b.name),
        |a, b| a.name == b.name && same_ast_arguments(&a.arguments, &b.arguments),
    )
}

fn get_ast_selection_key(
    selection: &ast::Selection,
    fragment_map: &HashMap<Name, Name>,
) -> SelectionKey {
    match selection {
        ast::Selection::Field(field) => SelectionKey::Field {
            response_name: field.response_name().clone(),
            directives: field.directives.clone(),
        },
        ast::Selection::FragmentSpread(fragment) => SelectionKey::FragmentSpread {
            fragment_name: fragment_map
                .get(&fragment.fragment_name)
                .unwrap_or(&fragment.fragment_name)
                .clone(),
            directives: fragment.directives.clone(),
        },
        ast::Selection::InlineFragment(fragment) => SelectionKey::InlineFragment {
            type_condition: fragment.type_condition.clone(),
            directives: fragment.directives.clone(),
        },
    }
}

fn same_ast_selection(
    x: &ast::Selection,
    y: &ast::Selection,
    fragment_map: &HashMap<Name, Name>,
) -> bool {
    match (x, y) {
        (ast::Selection::Field(x), ast::Selection::Field(y)) => {
            x.name == y.name
                && x.alias == y.alias
                && same_ast_arguments(&x.arguments, &y.arguments)
                && same_directives(&x.directives, &y.directives)
                && same_ast_selection_set_sorted(&x.selection_set, &y.selection_set, fragment_map)
        }
        (ast::Selection::FragmentSpread(x), ast::Selection::FragmentSpread(y)) => {
            let mapped_fragment_name = fragment_map
                .get(&x.fragment_name)
                .unwrap_or(&x.fragment_name);
            *mapped_fragment_name == y.fragment_name
                && same_directives(&x.directives, &y.directives)
        }
        (ast::Selection::InlineFragment(x), ast::Selection::InlineFragment(y)) => {
            x.type_condition == y.type_condition
                && same_directives(&x.directives, &y.directives)
                && same_ast_selection_set_sorted(&x.selection_set, &y.selection_set, fragment_map)
        }
        _ => false,
    }
}

fn hash_ast_selection_key(selection: &ast::Selection, fragment_map: &HashMap<Name, Name>) -> u64 {
    hash_value(&get_ast_selection_key(selection, fragment_map))
}

// Selections are sorted and compared after renaming x's fragment spreads according to the
// fragment_map.
fn same_ast_selection_set_sorted(
    x: &[ast::Selection],
    y: &[ast::Selection],
    fragment_map: &HashMap<Name, Name>,
) -> bool {
    fn sorted_by_selection_key<'a>(
        s: &'a [ast::Selection],
        fragment_map: &HashMap<Name, Name>,
    ) -> Vec<&'a ast::Selection> {
        let mut sorted: Vec<&ast::Selection> = s.iter().collect();
        sorted.sort_by_key(|x| hash_ast_selection_key(x, fragment_map));
        sorted
    }

    if x.len() != y.len() {
        return false;
    }
    let x_sorted = sorted_by_selection_key(x, fragment_map); // Map fragment spreads
    let y_sorted = sorted_by_selection_key(y, &Default::default()); // Don't map fragment spreads
    x_sorted
        .into_iter()
        .zip(y_sorted)
        .all(|(x, y)| same_ast_selection(x, y, fragment_map))
}

//==================================================================================================
// Unit tests

#[cfg(test)]
mod ast_comparison_tests {
    use super::*;

    #[test]
    fn test_query_variable_decl_order() {
        let op_x = r#"query($qv2: String!, $qv1: Int!) { x(arg1: $qv1, arg2: $qv2) }"#;
        let op_y = r#"query($qv1: Int!, $qv2: String!) { x(arg1: $qv1, arg2: $qv2) }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y).is_ok());
    }

    #[test]
    fn test_query_variable_decl_enum_value_coercion() {
        // Note: JS QP converts enum default values into strings.
        let op_x = r#"query($qv1: E! = "default_value") { x(arg1: $qv1) }"#;
        let op_y = r#"query($qv1: E! = default_value) { x(arg1: $qv1) }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y).is_ok());
    }

    #[test]
    fn test_query_variable_decl_object_value_coercion_empty_case() {
        // Note: Rust QP expands empty object default values by filling in its default field
        // values.
        let op_x = r#"query($qv1: T! = {}) { x(arg1: $qv1) }"#;
        let op_y =
            r#"query($qv1: T! = { field1: true, field2: "default_value" }) { x(arg1: $qv1) }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y).is_ok());
    }

    #[test]
    fn test_query_variable_decl_object_value_coercion_non_empty_case() {
        // Note: Rust QP expands an object default values by filling in its default field values.
        let op_x = r#"query($qv1: T! = {field1: true}) { x(arg1: $qv1) }"#;
        let op_y =
            r#"query($qv1: T! = { field1: true, field2: "default_value" }) { x(arg1: $qv1) }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y).is_ok());
    }

    #[test]
    fn test_query_variable_decl_list_of_object_value_coercion() {
        // Testing a combination of list and object value coercion.
        let op_x = r#"query($qv1: [T!]! = [{}]) { x(arg1: $qv1) }"#;
        let op_y =
            r#"query($qv1: [T!]! = [{field1: true, field2: "default_value"}]) { x(arg1: $qv1) }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y).is_ok());
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
        assert!(super::same_ast_document(&ast_x, &ast_y).is_ok());
    }

    #[test]
    fn test_top_level_selection_order() {
        let op_x = r#"{ x { w z } y }"#;
        let op_y = r#"{ y x { z w } }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y).is_ok());
    }

    #[test]
    fn test_fragment_definition_order() {
        let op_x = r#"{ q { ...f1 ...f2 } } fragment f1 on T { x y } fragment f2 on T { w z }"#;
        let op_y = r#"{ q { ...f1 ...f2 } } fragment f2 on T { w z } fragment f1 on T { x y }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y).is_ok());
    }

    #[test]
    fn test_selection_argument_is_compared() {
        let op_x = r#"{ x(arg1: "one") }"#;
        let op_y = r#"{ x(arg1: "two") }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y).is_err());
    }

    #[test]
    fn test_selection_argument_order() {
        let op_x = r#"{ x(arg1: "one", arg2: "two") }"#;
        let op_y = r#"{ x(arg2: "two", arg1: "one") }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y).is_ok());
    }

    #[test]
    fn test_selection_directive_order() {
        let op_x = r#"{ x @include(if:true) @skip(if:false) }"#;
        let op_y = r#"{ x @skip(if:false) @include(if:true) }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y).is_ok());
    }

    #[test]
    fn test_string_to_id_coercion_difference() {
        // JS QP coerces strings into integer for ID type, while Rust QP doesn't.
        // This tests a special case that same_ast_document accepts this difference.
        let op_x = r#"{ x(id: 123) }"#;
        let op_y = r#"{ x(id: "123") }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y).is_ok());
    }

    #[test]
    fn test_fragment_definition_different_names() {
        let op_x = r#"{ q { ...f1 ...f2 } } fragment f1 on T { x y } fragment f2 on T { w z }"#;
        let op_y = r#"{ q { ...g1 ...g2 } } fragment g1 on T { x y } fragment g2 on T { w z }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y).is_ok());
    }

    #[test]
    fn test_fragment_definition_different_names_nested_1() {
        // Nested fragments have the same name, only top-level fragments have different names.
        let op_x = r#"{ q { ...f2 } } fragment f1 on T { x y } fragment f2 on T { z ...f1 }"#;
        let op_y = r#"{ q { ...g2 } } fragment f1 on T { x y } fragment g2 on T { z ...f1 }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y).is_ok());
    }

    #[test]
    fn test_fragment_definition_different_names_nested_2() {
        // Nested fragments have different names.
        let op_x = r#"{ q { ...f2 } } fragment f1 on T { x y } fragment f2 on T { z ...f1 }"#;
        let op_y = r#"{ q { ...g2 } } fragment g1 on T { x y } fragment g2 on T { z ...g1 }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y).is_ok());
    }

    #[test]
    fn test_fragment_definition_different_names_nested_3() {
        // Nested fragments have different names.
        // Also, fragment definitions are in different order.
        let op_x = r#"{ q { ...f2 ...f3 } } fragment f1 on T { x y } fragment f2 on T { z ...f1 } fragment f3 on T { w } "#;
        let op_y = r#"{ q { ...g2 ...g3 } } fragment g1 on T { x y } fragment g2 on T { w }  fragment g3 on T { z ...g1 }"#;
        let ast_x = ast::Document::parse(op_x, "op_x").unwrap();
        let ast_y = ast::Document::parse(op_y, "op_y").unwrap();
        assert!(super::same_ast_document(&ast_x, &ast_y).is_ok());
    }
}

#[cfg(test)]
mod qp_selection_comparison_tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_requires_comparison_with_same_selection_key() {
        let requires_json = json!([
            {
                "kind": "InlineFragment",
                "typeCondition": "T",
                "selections": [
                    {
                        "kind": "Field",
                        "name": "id",
                    },
                  ]
            },
            {
                "kind": "InlineFragment",
                "typeCondition": "T",
                "selections": [
                    {
                        "kind": "Field",
                        "name": "id",
                    },
                    {
                        "kind": "Field",
                        "name": "job",
                    }
                  ]
            },
        ]);

        // The only difference between requires1 and requires2 is the order of selections.
        // But, their items all have the same SelectionKey.
        let requires1: Vec<Selection> = serde_json::from_value(requires_json).unwrap();
        let requires2: Vec<Selection> = requires1.iter().rev().cloned().collect();

        // `same_selection_set_sorted` fails to match, since it doesn't account for
        // two items with the same SelectionKey but in different order.
        assert!(!same_selection_set_sorted(&requires1, &requires2));
        // `same_requires` should succeed.
        assert!(same_requires(&requires1, &requires2));
    }
}
