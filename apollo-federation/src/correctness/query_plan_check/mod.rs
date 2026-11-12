//! Query plan correctness, ported from the Lean `checkQueryPlan` model.
//!
//! Port of `Apollo/Implementations/QueryPlanChecker.lean` in `apollo-graphql-lean`, where
//! `CheckQueryPlanCorrect` states that the checker accepts exactly the plans that are complete and
//! sound, and `checkQueryPlan_correct` proves it.
//!
//! The check is two halves:
//!
//! ```text
//! let fetched = plan.walk();
//! includes(operation_with(fetched), operation) && fetched.requirements_met
//! ```
//!
//! - **Completeness** — walk the plan once accumulating what it fetches, mount each fetch's
//!   selections at the path it runs at, then ask whether the result includes the client operation.
//!   That question is [`super::query_compare::includes`], itself a port of the model's
//!   `includesBool`.
//! - **Soundness** — every fetch is sent to a subgraph that is there and selects only what that
//!   subgraph resolves; every entity fetch's entries are read as they are sent and read only what
//!   can be read, every entity case is met, and the `__typename` its flatten path tests and the
//!   contextual values it reads are already fetched. Each is checked where the fetch is reached,
//!   while the state that reached it is in hand. No fetch site is ever collected, and the model's
//!   passes over every fetch of the plan (`fetchSubgraphsKnownBool`, `fetchesSelectResolvableBool`)
//!   are made at each fetch instead.
//!
//! Every comparison is made modulo what the plan's fetches return (`returned`): below each field,
//! nothing is owed at an object type no fetch writing that field at that position can return.
//!
//! # What a fetch must already have found
//!
//! A `requires` entry is not the authority on that; the demand and the input rewrites are. An
//! object's representation is every entry whose type condition admits it, merged, so the entries
//! are checked together, one entity case and object type at a time, in every configuration of
//! their `@skip`/`@include` conditions, through a resolvable `@key` of the type each object is sent
//! as: that `__typename` is the case's type, or a type that includes the object in the subgraph as
//! the case does, such as an entity interface over it; what each entry must already have fetched
//! of the demand is readable; and what the entries send covers the demand as narrowed by their
//! conditions. An input value setter may set nothing but that `__typename`; the planner writes one
//! to send an interface's or interface object's implementations under the interface's name. What
//! is fetched and sent is read off the demand by what the entry reads and sends
//! (`representation`); the entry adds nothing. A planner may fetch a `@requires` input under
//! an alias and name it back with an input key renamer, which is what the router's
//! `execute_selection_set` and `apply_rewrites` do between them.
//!
//! # Differences from the model
//!
//! - **The demand is narrowed by a path constraint, not rewritten.** The model narrows the demanded
//!   side of each comparison into a new operation (`narrowDemand`) because its inclusion decider
//!   takes no path constraint; `query_compare` takes one, so the returned types are passed to it.
//! - **Each key's demand is computed once per object type**, not once per configuration: only the
//!   condition varies across configurations.
//! - **What is compared is collapsed first.** What an entry must already have fetched, what the
//!   entries send and the narrowed demand each nest the entry's, the narrowing's and the demand's
//!   fragments on one type, which the inclusion decider pays for. A directive-free fragment on the
//!   type of the fragment enclosing it is spliced into it, and adjacent sibling fragments on one
//!   type under the same directives are merged, which changes nothing inclusion reads
//!   (`selections::collapse_fragments`). Coverage is then shortcut where what is sent equals the
//!   narrowed demand.
//! - **The key comparison and what a fetch can read are built once per buffer**, and shared by
//!   every entry and every fetch reading that buffer (`representation::ReadableKeyIndex`).
//! - **What a deferred block's dependencies sent is recorded, not recomputed.** The model walks
//!   the primary block a second time, in `PrimaryDeferBlock.dependencySelections`, to find what
//!   the fetches a deferred block `depends` on sent it. Each fetch's response is what the walk
//!   already returns for it, so it is recorded under the fetch's id as the walk passes, as
//!   `FetchNode::deferred_fetches` sends it, guarded only by the condition branches inside the
//!   primary block.
//! - **Each distinct copy of a key, once.** A reproduced copy repeating one already reproduced
//!   at its level exactly, source locations aside, is kept once, in first-occurrence order
//!   (`selections::selections_at`). The model's `selectionsUnderKey` keeps every copy, and
//!   inclusion reads equal ones as one; kept, they double along a chain of fetches at one site.
//! - **Literals are compared as coerced.** The model compares argument values as written and
//!   does not model input coercion. Before comparing, the client operation, every fetch's
//!   operation and every field set are coerced the way the planner coerces its fetch operations,
//!   and their `Float` and `ID` literals are spelled alike, so `1` and `1.0` of a `Float` are one
//!   value (`coerced_for_comparison`). Defaults are not filled in: a subgraph applies its own.
//! - **`@context` data availability.** Every contextual value a fetch reads must already be
//!   fetched wherever the fetch runs. The legacy checker does not check this at all: at run time
//!   a rewrite path that reaches nothing simply drops the argument, so nothing notices.
//! - **Validity** and **condition variables** — the two sections below.
//!
//! # Validity of the operations the walk builds
//!
//! The model assumes the operations it compares are valid and carries that as a premise of its
//! correctness statement, `QueryPlanWellFormed`, naming four: the plan read as an operation, what
//! each entity fetch has already fetched, what each `@key` demands of each entity case as its
//! entries send it, narrowed by their conditions and as each must already have fetched it, and
//! `__typename` mounted at each type-conditioned element of its path. It also takes every fetch's
//! own operation to be valid against its subgraph (`FetchOperationsValid`), as parsing it against
//! that subgraph already requires. The checker does not decide any of them.
//!
//! The first two are read off the buffer of what the plan has fetched, and that buffer is a
//! *response shape written in selection syntax*: it keys a response name by type condition and has
//! no field-merging rule, where an operation does. When a fetch's output rewrites rename a key the
//! planner had aliased apart — which it does exactly when two disjoint entity types need the same
//! `@requires` field at incompatible types — the two meet under one name and `FieldsInSetCanMerge`
//! rejects a plan whose data never collides. The legacy checker does not hit this because it
//! renames inside a response shape, where the two never meet.
//!
//! The third is assembled out of `@key` and `@requires` field sets, which supergraph composition
//! has already validated, so re-checking them here would only cost time, and the fourth is one
//! field mounted where the plan already reaches.
//!
//! # Unfetched introspection
//!
//! The client operation is compared with some introspection removed, mirroring the legacy
//! checker's policy. The planner ignores `__schema` and `__type`, so no plan ever fetches them.
//! The router answers a *root* `__typename` without asking a subgraph, so no fetch carries it;
//! below the root it is fetched like any other field and is compared. This is query-plan policy
//! rather than part of the inclusion relation, which is why it lives here and not in
//! `query_compare`.
//!
//! # Condition variables
//!
//! A variable a condition node branches on, or a `requires` entry's condition reads, is declared
//! `Boolean!` in every operation the checker builds, the client operation included, and loses its
//! default. The guards synthesized for a
//! condition branch are `@skip`/`@include`, whose `if:` argument is `Boolean!` — but the branch
//! may come from `@defer(if:)`, whose argument is nullable. Executing a condition node reads the
//! value as a Boolean and the inclusion relation case-splits over true and false, so tightening
//! the declaration changes nothing either decides; it is only what makes the synthesized guard
//! well-typed. Both sides get the same treatment, so they still agree on what they share.

mod context;
mod representation;
mod requires;
mod resolvable;
mod returned;
mod selections;
mod subgraph;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable;
use apollo_compiler::executable::FragmentMap;
use apollo_compiler::executable::Selection;
use apollo_compiler::name;
use apollo_compiler::validation::Valid;

use self::context::ContextSite;
use self::context::ContextUse;
use self::context::check_context_arguments_bound;
use self::context::check_context_declared;
use self::context::check_context_rewrites;
use self::context::check_root_fetch_uses_no_context;
use self::context::context_uses;
use self::context::context_variables;
use self::context::display_path;
use self::context::remove_context_arguments;
use self::representation::ReadableKeyIndex;
use self::representation::SchemaTypeLookup;
use self::representation::entry_configurations;
use self::representation::input_rewrites_idempotent;
use self::representation::input_rewrites_separated;
use self::representation::input_rewrites_well_formed;
use self::representation::narrowed_by_entry_condition;
use self::representation::narrowed_by_entry_conditions;
use self::representation::overwritten_keys;
use self::representation::required_selections;
use self::representation::sent_selections;
use self::representation::sent_typename;
use self::representation::writes_under_its_own_type;
use self::requires::Reading;
use self::requires::check_requires_conflict;
use self::requires::entity_fetch_requirement;
use self::requires::entry_selections;
use self::requires::requires_of_selections;
use self::requires::same_keys;
use self::resolvable::check_fetch_selects_resolvable;
use self::returned::ReturnedTypes;
use self::selections::apply_key_renamers;
use self::selections::apply_output_renamers;
use self::selections::collapse_fragments;
use self::selections::inline_fragment_spreads;
use self::selections::narrow_type_conditions;
use self::selections::selections_at;
use self::selections::type_filter_meets;
use self::selections::under_condition;
use self::subgraph::Subgraph;
use super::query_compare;
use super::query_compare::conditions::BooleanLiteral;
use super::response_shape_compare::ComparisonError;
use crate::compat::coerce_executable_values_for_comparison;
use crate::correctness::CheckerOptions;
use crate::query_plan::DeferredDependency;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchNode;
use crate::query_plan::PlanNode;
use crate::query_plan::QueryPlan;
use crate::query_plan::TopLevelPlanNode;
use crate::query_plan::requires_selection;
use crate::schema::ValidFederationSchema;
use crate::schema::position::INTROSPECTION_TYPENAME_FIELD_NAME;

//==================================================================================================
// Walking the plan
//==================================================================================================

/// What a walk of the plan produced.
struct Walked {
    /// Everything the plan fetches, as one selection set mounted at the query root. Order is
    /// load-bearing: append, never reorder.
    fetched: Vec<Selection>,
}

/// The state a fetch is reached with.
#[derive(Clone, Copy)]
struct Reached<'a> {
    /// Where the fetch runs, as a path of response keys from the query root.
    path: &'a [FetchDataPathElement],
    /// The condition-node branches in effect, read as a conjunction.
    condition: &'a [BooleanLiteral],
    /// Where the fetch records its own response, when it runs in the primary block of a `defer`
    /// node whose deferred blocks may name it as a dependency.
    deferred: Option<&'a DeferredFetches>,
}

/// What each fetch in a `defer` node's primary block sends the deferred blocks that name it.
///
/// Recorded as the walk passes each fetch, since `FetchNode::deferred_fetches` sends a fetch's own
/// response rather than everything fetched before it. A nested `defer` node keeps its own, so no
/// fetch inside one is recorded here.
struct DeferredFetches {
    /// The condition branches in force at the `defer` node. A response is guarded only by the
    /// branches inside the primary block; the ones above guard the deferred blocks as well.
    outer_conditions: usize,
    /// Each fetch's id and response, in the order the walk passed them.
    responses: std::cell::RefCell<Vec<(u64, Vec<Selection>)>>,
}

impl DeferredFetches {
    fn new(outer_conditions: usize) -> Self {
        DeferredFetches {
            outer_conditions,
            responses: Default::default(),
        }
    }

    /// What the fetches a deferred block depends on sent it, in the order they ran. `depends`
    /// holds fetch ids as strings.
    fn responses_of(&self, depends: &[DeferredDependency]) -> Vec<Selection> {
        self.responses
            .borrow()
            .iter()
            .filter(|(id, _)| {
                let id = id.to_string();
                depends.iter().any(|dependency| dependency.id == id)
            })
            .flat_map(|(_, response)| response.iter().cloned())
            .collect()
    }
}

pub(crate) struct Checker<'a> {
    supergraph_schema: &'a ValidFederationSchema,
    /// The schema index every inclusion test runs against. Built once: it is derived from the
    /// schema alone, and rebuilding it per test costs more than the tests themselves on a
    /// supergraph with many types.
    comparator: query_compare::QueryComparator<'a>,
    /// Read for each fetch's `@key` and `@requires` declarations.
    subgraphs_by_name: &'a IndexMap<Arc<str>, ValidFederationSchema>,
    /// Every operation the walk builds inherits its declarations and operation type.
    operation: &'a executable::Operation,
    /// What the plan's fetches return, which every inclusion test narrows its demanded side to.
    returned: ReturnedTypes,
    /// The supergraph's possible and field result types, as the key comparison reads them.
    supergraph_types: SchemaTypeLookup<'a>,
    /// What the last entity fetch could read, as an operation and as a key index. Fetches run in
    /// parallel read the same buffer, and building either reads all of it.
    last_readable: std::cell::RefCell<Option<ReadableBuffer>>,
    /// Every variable a condition node of this plan branches on or a `requires` entry reads.
    condition_variables: IndexSet<Name>,
    root_type: Name,
    /// The checks that are off unless the caller asks for them.
    options: CheckerOptions,
}

/// One buffer of what a fetch can read, viewed as the operation every requirement is compared with
/// and as the key index entries are read against.
struct ReadableBuffer {
    /// The buffer's top-level selections, by identity. Selections are immutable shared nodes, so
    /// the same nodes in the same order are the same buffer; holding them keeps them from being
    /// freed and their addresses reused.
    top_level: Vec<Selection>,
    as_operation: std::rc::Rc<Valid<ExecutableDocument>>,
    key_index: std::rc::Rc<ReadableKeyIndex>,
}

/// Whether two selections are the same node.
fn same_node(left: &Selection, right: &Selection) -> bool {
    match (left, right) {
        (Selection::Field(left), Selection::Field(right)) => Node::ptr_eq(left, right),
        (Selection::InlineFragment(left), Selection::InlineFragment(right)) => {
            Node::ptr_eq(left, right)
        }
        (Selection::FragmentSpread(left), Selection::FragmentSpread(right)) => {
            Node::ptr_eq(left, right)
        }
        _ => false,
    }
}

//==================================================================================================
// Checker construction
//==================================================================================================

impl<'a> Checker<'a> {
    fn new(
        supergraph_schema: &'a ValidFederationSchema,
        subgraphs_by_name: &'a IndexMap<Arc<str>, ValidFederationSchema>,
        operation: &'a executable::Operation,
        plan: &QueryPlan,
        root_type: Name,
        options: CheckerOptions,
    ) -> Result<Self, ComparisonError> {
        Ok(Checker {
            supergraph_schema,
            comparator: query_compare::QueryComparator::new(supergraph_schema)
                .map_err(|e| ComparisonError::new(e.to_string()))?,
            subgraphs_by_name,
            operation,
            returned: ReturnedTypes::of_plan(supergraph_schema, subgraphs_by_name, plan)?,
            supergraph_types: SchemaTypeLookup::new(supergraph_schema),
            last_readable: Default::default(),
            condition_variables: condition_variables(plan),
            root_type,
            options,
        })
    }
}

/// What an entity fetch selects where it runs: `body`, wherever a typed `requires` entry writes a
/// key ([`narrowed_by_entry_condition`]). What no entry covers reaches nothing and so contributes
/// nothing, which is how a fetch covering only some of an interface object's implementations
/// resolves nothing for the rest; and where an entry writes no key the router sends no
/// representation, so nothing is fetched there.
///
/// Conditions are compared by name, not by the object types they include, which is a shortcut:
/// where every entry writes its keys under its own unconditioned fragment, a `body` whose
/// selections are all fragments on the entries' types is already restricted and is returned
/// unchanged. That is the common case and the one worth recognizing, the alternative leaving the
/// rest of the checker a copy of `body` per entry to walk.
fn restrict_to_entries(
    requires: &[requires_selection::Selection],
    require_types: &[&Name],
    body: Vec<Selection>,
) -> Vec<Selection> {
    if body.is_empty() {
        return body;
    }
    let body_already_restricted = body.iter().all(|selection| match selection {
        Selection::InlineFragment(fragment) => fragment
            .type_condition
            .as_ref()
            .is_some_and(|type_condition| require_types.contains(&type_condition)),
        _ => false,
    });
    if body_already_restricted && requires.iter().all(writes_under_its_own_type) {
        return body;
    }
    requires
        .iter()
        .zip(require_types)
        .flat_map(|(entry, require_type)| {
            narrowed_by_entry_condition(entry, require_type, body.clone())
        })
        .collect()
}

/// Reports a flatten path element narrowed to no runtime type at all.
///
/// The planner writes an empty type condition — `…@|[]` — when it has worked out that the
/// position admits nothing, which says the fetch under the path can never run. The judgement is
/// right; emitting the fetch anyway is not. The rest of this checker is silent about it, and
/// correctly so: a path element that reaches no type mounts no requirement and admits no
/// fragment, so the fetch demands nothing and contributes nothing. That makes it dead code in the
/// plan rather than a soundness violation, which is why
/// [`CheckerOptions::check_empty_flatten_path_type_condition`] can turn it off.
fn check_path_reaches_a_type(path: &[FetchDataPathElement]) -> Result<(), ComparisonError> {
    for element in path {
        let condition = match element {
            FetchDataPathElement::Key(_, condition) | FetchDataPathElement::AnyIndex(condition) => {
                condition
            }
            FetchDataPathElement::TypenameEquals(_) | FetchDataPathElement::Parent => continue,
        };
        if condition.as_ref().is_some_and(|types| types.is_empty()) {
            return Err(ComparisonError::new(format!(
                "flatten path `{}` narrows `{element}` to no runtime type, \
                 so the fetch under it can never run",
                display_path(path)
            )));
        }
    }
    Ok(())
}

/// Every variable a condition node of a plan branches on, or a `requires` entry's condition reads.
fn condition_variables(plan: &QueryPlan) -> IndexSet<Name> {
    /// The variables a fetch's `requires` entries' conditions read.
    fn entry_variables(fetch: &FetchNode, out: &mut IndexSet<Name>) {
        fn collect(selection: &requires_selection::Selection, out: &mut IndexSet<Name>) {
            match selection {
                requires_selection::Selection::Field(field) => {
                    field.selections.iter().for_each(|s| collect(s, out))
                }
                requires_selection::Selection::InlineFragment(fragment) => {
                    for condition in &fragment.conditions {
                        if let requires_selection::ConditionValue::Variable(name) = &condition.if_ {
                            out.insert(name.clone());
                        }
                    }
                    fragment.selections.iter().for_each(|s| collect(s, out))
                }
            }
        }
        fetch.requires.iter().for_each(|entry| collect(entry, out));
    }

    fn walk(node: &PlanNode, out: &mut IndexSet<Name>) {
        match node {
            PlanNode::Fetch(fetch) => entry_variables(fetch, out),
            PlanNode::Sequence(sequence) => sequence.nodes.iter().for_each(|n| walk(n, out)),
            PlanNode::Parallel(parallel) => parallel.nodes.iter().for_each(|n| walk(n, out)),
            PlanNode::Flatten(flatten) => walk(&flatten.node, out),
            PlanNode::Defer(defer) => {
                defer.primary.node.iter().for_each(|n| walk(n, out));
                defer
                    .deferred
                    .iter()
                    .filter_map(|block| block.node.as_ref())
                    .for_each(|n| walk(n, out));
            }
            PlanNode::Condition(node) => {
                out.insert(node.condition_variable.clone());
                node.if_clause.iter().for_each(|n| walk(n, out));
                node.else_clause.iter().for_each(|n| walk(n, out));
            }
        }
    }

    let mut out = IndexSet::default();
    match &plan.node {
        None => {}
        Some(TopLevelPlanNode::Subscription(subscription)) => {
            entry_variables(&subscription.primary, &mut out);
            subscription.rest.iter().for_each(|n| walk(n, &mut out));
        }
        Some(TopLevelPlanNode::Fetch(fetch)) => entry_variables(fetch, &mut out),
        Some(TopLevelPlanNode::Sequence(node)) => {
            node.nodes.iter().for_each(|n| walk(n, &mut out));
        }
        Some(TopLevelPlanNode::Parallel(node)) => {
            node.nodes.iter().for_each(|n| walk(n, &mut out));
        }
        Some(TopLevelPlanNode::Flatten(node)) => walk(&node.node, &mut out),
        Some(TopLevelPlanNode::Defer(node)) => {
            node.primary.node.iter().for_each(|n| walk(n, &mut out));
            node.deferred
                .iter()
                .filter_map(|block| block.node.as_ref())
                .for_each(|n| walk(n, &mut out));
        }
        Some(TopLevelPlanNode::Condition(node)) => {
            out.insert(node.condition_variable.clone());
            node.if_clause.iter().for_each(|n| walk(n, &mut out));
            node.else_clause.iter().for_each(|n| walk(n, &mut out));
        }
    }
    out
}

//==================================================================================================
// Walking node tree
//==================================================================================================

impl<'a> Checker<'a> {
    /// Walks a plan node once, returning what it adds to `available`.
    ///
    /// Each fetch is checked where it is reached, while the state that reached it is in hand, so
    /// no fetch site is ever collected. The threading is the model's: a sequence's children run in
    /// order, parallel children all see the same input, a defer node's deferred blocks start after
    /// its primary block.
    ///
    /// Two states are threaded. `available` is everything fetched before the node, and is what a
    /// contribution or a requirement is mounted into; `readable` is what of that the node can
    /// read, and is what a requirement is compared with. They differ only inside a deferred
    /// block.
    fn walk_node(
        &self,
        node: &PlanNode,
        reached: Reached<'_>,
        available: &[Selection],
        readable: &[Selection],
    ) -> Result<Vec<Selection>, ComparisonError> {
        match node {
            PlanNode::Fetch(fetch) => {
                let contributed = self.walk_fetch(fetch, reached, available, readable)?;
                if let (Some(deferred), Some(id)) = (reached.deferred, fetch.id) {
                    deferred.responses.borrow_mut().push((
                        id,
                        under_condition(
                            &reached.condition[deferred.outer_conditions..],
                            &self.root_type,
                            contributed.clone(),
                        ),
                    ));
                }
                Ok(contributed)
            }
            PlanNode::Sequence(sequence) => {
                // Each child sees, and can read, what the previous ones added.
                let mut added: Vec<Selection> = Vec::new();
                for child in &sequence.nodes {
                    let mut seen = available.to_vec();
                    seen.extend(added.iter().cloned());
                    let mut seen_readable = readable.to_vec();
                    seen_readable.extend(added.iter().cloned());
                    added.extend(self.walk_node(child, reached, &seen, &seen_readable)?);
                }
                Ok(added)
            }
            PlanNode::Parallel(parallel) => {
                // Every child sees the same input.
                let mut added: Vec<Selection> = Vec::new();
                for child in &parallel.nodes {
                    added.extend(self.walk_node(child, reached, available, readable)?);
                }
                Ok(added)
            }
            PlanNode::Flatten(flatten) => {
                if self.options.check_empty_flatten_path_type_condition {
                    check_path_reaches_a_type(&flatten.path)?;
                }
                let mut path = reached.path.to_vec();
                path.extend(flatten.path.iter().cloned());
                self.walk_node(
                    &flatten.node,
                    Reached {
                        path: &path,
                        ..reached
                    },
                    available,
                    readable,
                )
            }
            PlanNode::Defer(defer) => {
                // The primary block's fetches record what they send, for the deferred blocks that
                // name them.
                let deferred_fetches = DeferredFetches::new(reached.condition.len());
                let primary = match &defer.primary.node {
                    Some(node) => self.walk_node(
                        node,
                        Reached {
                            deferred: Some(&deferred_fetches),
                            ..reached
                        },
                        available,
                        readable,
                    )?,
                    None => Vec::new(),
                };
                // Every deferred block starts after the whole primary block, so that is what was
                // fetched before it. What it can read is less: what the `defer` node could read,
                // and the responses of the fetches it depends on — or the whole primary block
                // when it names none, `DeferredNode::execute` then waiting for all of it.
                let mut block_available = available.to_vec();
                block_available.extend(primary.iter().cloned());
                let mut added = primary.clone();
                for block in &defer.deferred {
                    let Some(node) = &block.node else {
                        continue;
                    };
                    let mut block_readable = readable.to_vec();
                    if block.depends.is_empty() {
                        block_readable.extend(primary.iter().cloned());
                    } else {
                        block_readable.extend(deferred_fetches.responses_of(&block.depends));
                    }
                    added.extend(self.walk_node(
                        node,
                        Reached {
                            deferred: None,
                            ..reached
                        },
                        &block_available,
                        &block_readable,
                    )?);
                }
                Ok(added)
            }
            PlanNode::Condition(node) => {
                let mut added = Vec::new();
                let branches = [
                    (
                        &node.if_clause,
                        BooleanLiteral::Positive(node.condition_variable.clone()),
                    ),
                    (
                        &node.else_clause,
                        BooleanLiteral::Negative(node.condition_variable.clone()),
                    ),
                ];
                for (clause, literal) in branches {
                    let Some(clause) = clause else {
                        continue;
                    };
                    let mut condition = reached.condition.to_vec();
                    condition.push(literal.clone());
                    let inner = self.walk_node(
                        clause,
                        Reached {
                            condition: &condition,
                            ..reached
                        },
                        available,
                        readable,
                    )?;
                    added.extend(under_condition(
                        std::slice::from_ref(&literal),
                        &self.root_type,
                        inner,
                    ));
                }
                Ok(added)
            }
        }
    }

    /// Walks a root plan node. A subscription plan is read as a query, as
    /// `interpret_subscription_node` reads it.
    fn walk_top_level(&self, node: &TopLevelPlanNode) -> Result<Walked, ComparisonError> {
        let reached = Reached {
            path: &[],
            condition: &[],
            deferred: None,
        };
        let fetched = match node {
            TopLevelPlanNode::Subscription(subscription) => {
                // The primary fetch opens the stream, so it precedes the rest of the plan.
                let primary = self.walk_fetch(&subscription.primary, reached, &[], &[])?;
                let mut added = primary.clone();
                if let Some(rest) = &subscription.rest {
                    added.extend(self.walk_node(rest, reached, &primary, &primary)?);
                }
                added
            }
            TopLevelPlanNode::Fetch(fetch) => {
                self.walk_node(&PlanNode::Fetch(fetch.clone()), reached, &[], &[])?
            }
            TopLevelPlanNode::Sequence(node) => {
                self.walk_node(&PlanNode::Sequence(node.clone()), reached, &[], &[])?
            }
            TopLevelPlanNode::Parallel(node) => {
                self.walk_node(&PlanNode::Parallel(node.clone()), reached, &[], &[])?
            }
            TopLevelPlanNode::Flatten(node) => {
                self.walk_node(&PlanNode::Flatten(node.clone()), reached, &[], &[])?
            }
            TopLevelPlanNode::Defer(node) => {
                self.walk_node(&PlanNode::Defer(node.clone()), reached, &[], &[])?
            }
            TopLevelPlanNode::Condition(node) => {
                self.walk_node(&PlanNode::Condition(node.clone()), reached, &[], &[])?
            }
        };
        Ok(Walked { fetched })
    }

    /// What one fetch contributes, mounted where it runs.
    ///
    /// An entity fetch is checked here, while the state that reached it is in hand, so no fetch
    /// site is ever collected.
    fn walk_fetch(
        &self,
        fetch: &FetchNode,
        reached: Reached<'_>,
        available: &[Selection],
        readable: &[Selection],
    ) -> Result<Vec<Selection>, ComparisonError> {
        // Every fetch, root fetches included, is sent to a subgraph that must be there and must
        // resolve what the fetch selects.
        let subgraph_schema = self.subgraph_schema(fetch)?;
        let (root_type, body) = self.fetch_body(fetch, subgraph_schema)?;
        check_fetch_selects_resolvable(&fetch.subgraph_name, subgraph_schema, &root_type, &body)?;
        // What the fetch can return under a fragment is what its subgraph admits there, which can
        // be less than the supergraph does. The entity cases are read off the operation as
        // written; what it contributes, off the narrowed one.
        let narrowed = narrow_type_conditions(self.supergraph_schema, subgraph_schema, &body);
        // A root fetch selects its whole operation and requires nothing; an entity fetch selects
        // what its operation asks of the entities it was given, which are those of a type some
        // `requires` entry is written at: `execute_selection_set` keeps an entry's fragment only
        // for an entity of a matching type, and an entity no entry matches gets an empty
        // representation, which is never sent. So the selections are kept once per entry type,
        // under that type. For an interface object, one subgraph type standing for every
        // implementation, a fetch whose entries cover only some of them resolves nothing for the
        // rest; `interpret_fetch_node` merges every `_entities` fragment regardless.
        let subgraph = Subgraph::new(subgraph_schema)?;
        // A Rust-only shortcut, with no counterpart in the Lean model: a subgraph that applies
        // `@fromContext` to no argument declares none for any field, so the walk could only come
        // back empty and every contextual-argument check holds vacuously. Skipping it changes no
        // verdict; it spares every other subgraph a second walk of each fetch's operation.
        let uses = if subgraph.declares_context_arguments() {
            context_uses(&subgraph, subgraph_schema, &root_type, &body)?
        } else {
            Vec::new()
        };
        let selected = if fetch.requires.is_empty() {
            check_root_fetch_uses_no_context(fetch, &uses)?;
            narrowed
        } else {
            self.check_fetch(
                fetch,
                &subgraph,
                &uses,
                &entity_body(fetch, &body)?,
                reached,
                available,
                readable,
            )?;
            restrict_to_entries(
                &fetch.requires,
                &require_types(fetch)?,
                entity_body(fetch, &narrowed)?,
            )
        };
        // The entity cases were read off the fetch's own operation above; what it *contributes*
        // is that selection set under the key renames its output rewrites apply and without the
        // synthetic arguments its context rewrites bind. `interpret_fetch_node` applies the two
        // in this order.
        // The renames start at the operation's root type for a root fetch; an entity fetch's
        // types come from its entries' fragments.
        let root_type = if fetch.requires.is_empty() {
            self.supergraph_schema
                .schema()
                .root_operation(fetch.operation_kind)
                .cloned()
        } else {
            None
        };
        let contributed = apply_output_renamers(
            self.supergraph_schema,
            root_type.as_ref(),
            &fetch.output_rewrites,
            selected,
        );
        let contributed =
            remove_context_arguments(&context_variables(&fetch.context_rewrites), contributed);
        Ok(selections_at(
            self.supergraph_schema,
            available,
            None,
            reached.path,
            &contributed,
        ))
    }

    /// The schema of the subgraph a fetch is sent to.
    fn subgraph_schema(
        &self,
        fetch: &FetchNode,
    ) -> Result<&'a ValidFederationSchema, ComparisonError> {
        self.subgraphs_by_name
            .get(fetch.subgraph_name.as_ref())
            .ok_or_else(|| {
                ComparisonError::new(format!(
                    "fetch to unknown subgraph `{}`",
                    fetch.subgraph_name
                ))
            })
    }

    /// A fetch's own operation, its fragment spreads inlined, and its root type in the subgraph's
    /// schema.
    fn fetch_body(
        &self,
        fetch: &FetchNode,
        subgraph_schema: &ValidFederationSchema,
    ) -> Result<(Name, Vec<Selection>), ComparisonError> {
        fetch_operation(fetch, subgraph_schema)
    }
}

/// `document` with its literals spelled as input coercion reads them
/// ([`coerce_executable_values_for_comparison`]), so that literals of one value compare equal:
/// `1` and `1.0` of a `Float`, `4` and `"4"` of an `ID`, `"a"` and `["a"]` of a list. The client
/// operation, every fetch's operation and every field set are read this way before they are
/// compared. Coercing changes no field or argument a document selects, only how a literal is
/// written, so it stays valid.
fn coerced_for_comparison(
    schema: &ValidFederationSchema,
    document: &Valid<ExecutableDocument>,
) -> Valid<ExecutableDocument> {
    let mut document = document.clone().into_inner();
    coerce_executable_values_for_comparison(schema.schema(), &mut document);
    Valid::assume_valid(document)
}

/// A fetch's own operation, its fragment spreads inlined, and its root type in the subgraph's
/// schema.
fn fetch_operation(
    fetch: &FetchNode,
    subgraph_schema: &ValidFederationSchema,
) -> Result<(Name, Vec<Selection>), ComparisonError> {
    let parsed = fetch.operation_document.as_parsed().map_err(|e| {
        ComparisonError::new(format!(
            "fetch to {}: operation document is not parsed: {e}",
            fetch.subgraph_name
        ))
    })?;
    let document = coerced_for_comparison(subgraph_schema, parsed);
    let operation = document.operations.get(None).map_err(|_| {
        ComparisonError::new(format!(
            "fetch to {}: expected exactly one operation",
            fetch.subgraph_name
        ))
    })?;
    let root_type = subgraph_schema
        .schema()
        .root_operation(operation.operation_type)
        .cloned()
        .ok_or_else(|| {
            ComparisonError::new(format!(
                "fetch to {}: the subgraph has no {} root",
                fetch.subgraph_name, operation.operation_type
            ))
        })?;
    // Spreads go here, at the boundary where this fetch's selections join the shared buffer;
    // see `inline_fragment_spreads`.
    Ok((
        root_type,
        inline_fragment_spreads(&operation.selection_set.selections, &document.fragments)?,
    ))
}

/// The federation entity entry point. An entity fetch's operation selects exactly this one root
/// field, and its subselections are what the fetch contributes where it was flattened to.
const ENTITIES_FIELD_NAME: &str = "_entities";

/// What an entity fetch asks of the entities it was given, with its shape checked.
///
/// This is the model's `entityOperationWellFormed`: exactly one root field, `_entities`. What is
/// selected under it must all be type-conditioned inline fragments, which [`entity_cases`] checks
/// where it reads them off.
fn entity_body(fetch: &FetchNode, body: &[Selection]) -> Result<Vec<Selection>, ComparisonError> {
    match body {
        [Selection::Field(field)] if field.name == ENTITIES_FIELD_NAME => {
            Ok(field.selection_set.selections.clone())
        }
        _ => Err(ComparisonError::new(format!(
            "fetch to {}: an entity fetch must select exactly one root field, `{ENTITIES_FIELD_NAME}`",
            fetch.subgraph_name
        ))),
    }
}

//==================================================================================================
// Checking one fetch's requirements
//==================================================================================================

/// One `requires` entry, with the type it is written at and its two readings.
struct Entry<'a> {
    item: &'a requires_selection::Selection,
    require_type: &'a Name,
    /// Each field under the key it is read from: its alias if it has one.
    as_read: Selection,
    /// Each field under its own name, which is what the subgraph reads it as.
    as_sent: Selection,
}

/// What one `@key` makes of an entity case at one object type: the demand, what each entry must
/// already have fetched of it, what the entries send of it, and the demand as narrowed by their
/// conditions. Only the condition varies across configurations, so this is computed once per key.
struct KeyDemand {
    required_by_entry: Vec<Vec<Selection>>,
    sent: Vec<Selection>,
    narrowed_demand: Vec<Selection>,
    /// Whether what the entries send is the narrowed demand itself, which covers itself wherever
    /// it is mounted.
    sent_is_narrowed_demand: bool,
}

impl Checker<'_> {
    /// Whether one entity fetch is well formed, every object type its entries send is resolved by
    /// an entity case, each entry is read as it is sent and only reads what can be read, every
    /// case is met, and the `__typename` its path tests and the contextual data it reads are
    /// already fetched.
    ///
    /// Context rewrites are an entity fetch's business: `add_context_renamers_for_selection_set`
    /// runs on the node that has just been given its key inputs, so a fetch with them has
    /// `requires`.
    #[allow(clippy::too_many_arguments)]
    fn check_fetch(
        &self,
        fetch: &FetchNode,
        subgraph: &Subgraph<'_>,
        uses: &[ContextUse],
        entity_body: &[Selection],
        reached: Reached<'_>,
        available: &[Selection],
        readable: &[Selection],
    ) -> Result<(), ComparisonError> {
        let fail = |message: String| {
            ComparisonError::new(format!("fetch to {}: {message}", fetch.subgraph_name))
        };
        let cases = entity_cases(fetch, entity_body)?;
        let require_types = require_types(fetch)?;
        let mut entries = Vec::with_capacity(require_types.len());
        for (item, require_type) in fetch.requires.iter().zip(&require_types) {
            let reading = |reading| -> Result<Selection, ComparisonError> {
                let mut selections = entry_selections(
                    self.supergraph_schema,
                    require_type,
                    reading,
                    std::slice::from_ref(item),
                )?;
                Ok(selections.pop().expect("an entry reads as one selection"))
            };
            entries.push(Entry {
                item,
                require_type,
                as_read: reading(Reading::AsRead)?,
                as_sent: reading(Reading::AsSent)?,
            });
        }

        // Every object type an entry sends is one an entity case resolves.
        let mut requires_types: Vec<Name> = Vec::new();
        for require_type in &require_types {
            for object_type in self.supergraph_types.possible_types(require_type).iter() {
                if !requires_types.contains(object_type) {
                    requires_types.push(object_type.clone());
                }
            }
        }
        for object_type in &requires_types {
            if !cases.iter().any(|(entity_type, _)| {
                self.supergraph_types
                    .possible_types(entity_type)
                    .contains(object_type)
            }) {
                return Err(fail(format!(
                    "`requires` entries send `{object_type}`, which no entity case resolves"
                )));
            }
        }

        // What the fetch can read, which every requirement is compared with. Bound once per
        // fetch, not per entry or per `@key`.
        let (readable_doc, readable_keys) = self.readable_buffer(readable);
        let readable_doc = &*readable_doc;

        for entry in &entries {
            self.check_entry_agrees_with_rewrites(fetch, entry)?;
            if let Some(key) = overwritten_keys(std::slice::from_ref(entry.item)).first() {
                return Err(fail(format!(
                    "`requires` entry {} overwrites `{key}`, which it read before",
                    entry.item
                )));
            }
            let read = self.required_at(
                reached,
                reached.condition,
                available,
                vec![entry.as_read.clone()],
            );
            readable_keys
                .includes(&self.supergraph_types, &self.returned, &read)
                .map_err(|reason| {
                    fail(format!(
                        "`requires` entry {} reads a key the plan has not fetched: {reason}",
                        entry.item
                    ))
                })?;
        }
        input_rewrites_well_formed(fetch).map_err(fail)?;
        input_rewrites_separated(self.supergraph_schema, fetch).map_err(fail)?;

        for (entity_type, entity_selections) in &cases {
            self.check_representation_meets_case(
                fetch,
                subgraph,
                &entries,
                &requires_types,
                entity_type,
                entity_selections,
                reached,
                available,
                readable_doc,
            )?;
        }
        self.check_typenames_readable(fetch, reached, available, readable_doc)?;
        check_context_arguments_bound(fetch, uses)?;
        check_context_rewrites(
            self.supergraph_schema,
            &self.root_type,
            reached.path,
            reached.condition,
            available,
            readable,
            fetch,
            &requires_types,
        )?;
        check_context_declared(
            &ContextSite {
                schema: self.supergraph_schema,
                root_type: &self.root_type,
                path: reached.path,
                available,
                readable,
                fetch,
                requires_types: &requires_types,
            },
            subgraph,
            uses,
        )
    }

    /// What a fetch can read, as an operation and as a key index, built once per buffer.
    fn readable_buffer(
        &self,
        readable: &[Selection],
    ) -> (
        std::rc::Rc<Valid<ExecutableDocument>>,
        std::rc::Rc<ReadableKeyIndex>,
    ) {
        if let Some(last) = self.last_readable.borrow().as_ref()
            && last.top_level.len() == readable.len()
            && last
                .top_level
                .iter()
                .zip(readable)
                .all(|(left, right)| same_node(left, right))
        {
            return (last.as_operation.clone(), last.key_index.clone());
        }
        let top_level = readable.to_vec();
        let as_operation = std::rc::Rc::new(self.operation_with(top_level.clone()));
        let key_index = std::rc::Rc::new(ReadableKeyIndex::new(
            &self.supergraph_types,
            &self.root_type,
            Arc::new(top_level.clone()),
        ));
        *self.last_readable.borrow_mut() = Some(ReadableBuffer {
            top_level,
            as_operation: as_operation.clone(),
            key_index: key_index.clone(),
        });
        (as_operation, key_index)
    }

    /// A requirement of a fetch, stated at the query root: mounted where the fetch runs and
    /// guarded by `condition`.
    fn required_at(
        &self,
        reached: Reached<'_>,
        condition: &[BooleanLiteral],
        available: &[Selection],
        requirement: Vec<Selection>,
    ) -> Vec<Selection> {
        under_condition(
            condition,
            &self.root_type,
            selections_at(
                self.supergraph_schema,
                available,
                None,
                reached.path,
                &requirement,
            ),
        )
    }

    /// Does `left` include `right`, narrowed to what the plan returns? `Err` explains why not.
    fn includes(
        &self,
        left: &Valid<ExecutableDocument>,
        right: Vec<Selection>,
    ) -> Result<(), String> {
        self.comparator
            .includes_with_constraint(
                &self.returned.constraint(),
                left,
                &self.operation_with(right),
            )
            .map_err(|e| e.to_string())
    }

    /// One entity case met for each object type it is sent: the object is sent with a `__typename`
    /// ([`sent_typename`]) that resolves to the case ([`Subgraph::sent_type_resolves_to_case`]), and
    /// in every configuration of the entries that can send that type, through a resolvable key of
    /// the sent type, the input rewrites are idempotent on its demand, what each of those entries
    /// must already have fetched is readable, and together they send the demand, narrowed by their
    /// conditions.
    #[allow(clippy::too_many_arguments)]
    fn check_representation_meets_case(
        &self,
        fetch: &FetchNode,
        subgraph: &Subgraph<'_>,
        entries: &[Entry<'_>],
        requires_types: &[Name],
        entity_type: &Name,
        entity_selections: &[Selection],
        reached: Reached<'_>,
        available: &[Selection],
        readable_doc: &Valid<ExecutableDocument>,
    ) -> Result<(), ComparisonError> {
        let fail = |message: String| {
            ComparisonError::new(format!("fetch to {}: {message}", fetch.subgraph_name))
        };
        let entity_objects = self.supergraph_types.possible_types(entity_type);
        let object_types: Vec<&Name> = requires_types
            .iter()
            .filter(|object_type| entity_objects.contains(object_type))
            .collect();
        if object_types.is_empty() {
            return Err(fail(format!(
                "entity case `{entity_type}` is sent no `requires` entry"
            )));
        }
        if self.options.check_requires_conflict {
            let requires = requires_of_selections(
                self.supergraph_schema,
                subgraph,
                entity_type,
                entity_type,
                entity_selections,
            )?;
            check_requires_conflict(self.supergraph_schema, &requires)
                .map_err(|e| fail(format!("entity case `{entity_type}`: {}", e.description())))?;
        }
        for object_type in object_types {
            let Some(sent) =
                sent_typename(self.supergraph_schema, &fetch.input_rewrites, object_type)
            else {
                return Err(fail(format!(
                    "entity case `{entity_type}`: a `{object_type}` is sent with a `__typename` \
                     that names no type"
                )));
            };
            if !subgraph.sent_type_resolves_to_case(&sent, object_type, entity_type) {
                return Err(fail(format!(
                    "entity case `{entity_type}`: a `{object_type}` is sent as `{sent}`, which \
                     resolves to no object the case applies to"
                )));
            }
            // The subgraph runs the sent type's reference resolver, so the key is read there.
            let keys = subgraph.keys(&sent)?;
            if keys.is_empty() {
                return Err(fail(format!(
                    "entity case `{entity_type}`: a `{object_type}` is sent as `{sent}`, on which \
                     the subgraph declares no `@key`"
                )));
            }
            let for_type: Vec<&Entry<'_>> = entries
                .iter()
                .filter(|entry| {
                    self.supergraph_types
                        .possible_types(entry.require_type)
                        .contains(object_type)
                })
                .collect();
            let items: Vec<&requires_selection::Selection> =
                for_type.iter().map(|entry| entry.item).collect();
            let requires = requires_of_selections(
                self.supergraph_schema,
                subgraph,
                entity_type,
                object_type,
                entity_selections,
            )?;

            // Each key's demand and what follows from it, once; `Err` is why the key cannot
            // serve whatever the configuration.
            let per_key: Vec<Result<KeyDemand, String>> = keys
                .iter()
                .map(|key| {
                    if !key.resolvable {
                        return Err("the subgraph will not resolve the entity from this key".into());
                    }
                    let demand = entity_fetch_requirement(
                        self.supergraph_schema,
                        object_type,
                        &key.fields,
                        &requires,
                    )
                    .map_err(|e| {
                        format!(
                            "does not apply to `{object_type}`:\n{}",
                            indent(&e.to_string())
                        )
                    })?;
                    if !input_rewrites_idempotent(self.supergraph_schema, fetch, &demand) {
                        return Err("the input rewrites rename a key the subgraph demands".into());
                    }
                    // As built, each of these nests the entry's, the narrowing's and the demand's
                    // fragments on one type; spliced into one, inclusion has that much less to
                    // read, and what is sent is usually the narrowed demand exactly.
                    let sent = collapse_fragments(
                        for_type
                            .iter()
                            .flat_map(|entry| {
                                sent_selections(entry.item, &entry.as_sent, object_type, &demand)
                            })
                            .collect(),
                    );
                    let narrowed = collapse_fragments(narrowed_by_entry_conditions(
                        &items,
                        object_type,
                        &demand,
                    ));
                    let sent_is_narrowed_demand = sent == narrowed;
                    Ok(KeyDemand {
                        required_by_entry: for_type
                            .iter()
                            .map(|entry| {
                                collapse_fragments(required_selections(
                                    fetch,
                                    entry.item,
                                    &entry.as_read,
                                    object_type,
                                    &demand,
                                ))
                            })
                            .collect(),
                        sent,
                        narrowed_demand: narrowed,
                        sent_is_narrowed_demand,
                    })
                })
                .collect();

            for configuration in entry_configurations(&items) {
                let mut condition = reached.condition.to_vec();
                condition.extend(configuration.iter().cloned());
                let mut unmet = Vec::new();
                let mut met = false;
                for (key, outcome) in keys.iter().zip(&per_key) {
                    let reason = match outcome {
                        Err(reason) => Some(reason.clone()),
                        Ok(key_demand) => self.key_unmet(
                            key_demand,
                            reached,
                            &condition,
                            available,
                            readable_doc,
                        )?,
                    };
                    match reason {
                        None => {
                            met = true;
                            break;
                        }
                        Some(reason) => {
                            unmet.push(format!("  @key({}): {}", key.fields, reason.trim_start()))
                        }
                    }
                }
                if !met {
                    let configuration = if configuration.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " when {}",
                            configuration
                                .iter()
                                .map(|literal| literal.to_string())
                                .collect::<Vec<_>>()
                                .join(" and ")
                        )
                    };
                    return Err(fail(format!(
                        "entity case `{entity_type}` is not met for `{object_type}`{configuration}:\n{}",
                        unmet.join("\n")
                    )));
                }
            }
        }
        Ok(())
    }

    /// Why one key's demand is not met under `condition`: `None` if what each entry must already
    /// have fetched is readable and what the entries send covers the demand.
    fn key_unmet(
        &self,
        key_demand: &KeyDemand,
        reached: Reached<'_>,
        condition: &[BooleanLiteral],
        available: &[Selection],
        readable_doc: &Valid<ExecutableDocument>,
    ) -> Result<Option<String>, ComparisonError> {
        for required in &key_demand.required_by_entry {
            let required = self.required_at(reached, condition, available, required.clone());
            if let Err(e) = self.includes(readable_doc, required) {
                return Ok(Some(format!(
                    "the plan has not fetched what the subgraph demands:\n{}",
                    indent(&e.to_string())
                )));
            }
        }
        if key_demand.sent_is_narrowed_demand {
            return Ok(None);
        }
        let sent_doc = self.operation_with(self.required_at(
            reached,
            condition,
            available,
            key_demand.sent.clone(),
        ));
        let narrowed = self.required_at(
            reached,
            condition,
            available,
            key_demand.narrowed_demand.clone(),
        );
        if let Err(e) = self.includes(&sent_doc, narrowed) {
            return Ok(Some(format!(
                "the entries do not send what the subgraph demands:\n{}",
                indent(&e.to_string())
            )));
        }
        Ok(None)
    }

    /// Every `__typename` the router tests along an entity fetch's path is there for it to read
    /// wherever the fetch runs.
    ///
    /// `iterate_path` follows a type-conditioned path element into an object only when its
    /// `__typename` is one the condition names, and skips one with none, so a fetch below a
    /// condition whose `__typename` the plan never fetched runs nowhere. Mounted at each
    /// conditioned element, where the condition puts it on the objects it names, `__typename` must
    /// be in what the fetch can read; an object of a type the condition does not name is skipped
    /// whether or not its `__typename` is there, so it owes none.
    fn check_typenames_readable(
        &self,
        fetch: &FetchNode,
        reached: Reached<'_>,
        available: &[Selection],
        readable_doc: &Valid<ExecutableDocument>,
    ) -> Result<(), ComparisonError> {
        let typename = self.typename_selection()?;
        for prefix in typename_inspected_paths(reached.path) {
            let required = under_condition(
                reached.condition,
                &self.root_type,
                selections_at(
                    self.supergraph_schema,
                    available,
                    None,
                    prefix,
                    std::slice::from_ref(&typename),
                ),
            );
            if let Err(e) = self.includes(readable_doc, required) {
                return Err(ComparisonError::new(format!(
                    "fetch to {}: the flatten path tests `__typename` at `{}`, which the plan has \
                     not fetched there:\n{}",
                    fetch.subgraph_name,
                    display_path(prefix),
                    indent(&e.to_string())
                )));
            }
        }
        Ok(())
    }

    /// The `__typename` field, as a plan selects it.
    fn typename_selection(&self) -> Result<Selection, ComparisonError> {
        let definition = self
            .supergraph_schema
            .schema()
            .type_field(&self.root_type, &INTROSPECTION_TYPENAME_FIELD_NAME)
            .map_err(|_| ComparisonError::new("no `__typename` definition".to_string()))?;
        Ok(executable::Field::new(
            INTROSPECTION_TYPENAME_FIELD_NAME.clone(),
            definition.clone(),
        )
        .into())
    }

    /// A `requires` entry agrees with the input rewrites: the entry as read (`AsRead`, each field
    /// under the key it is read from), renamed forwards as `apply_rewrites` renames a
    /// representation, has the keys it is sent under (`AsSent`, each under its own name), compared
    /// key for key. No alias is left unnamed, and no demanded field is renamed away.
    ///
    /// Two fields sent under one key are left to [`input_rewrites_separated`], and what must be
    /// fetched is taken back through the rewrites as the entry reads it ([`required_selections`]).
    fn check_entry_agrees_with_rewrites(
        &self,
        fetch: &FetchNode,
        entry: &Entry<'_>,
    ) -> Result<(), ComparisonError> {
        let as_sent = std::slice::from_ref(&entry.as_sent);
        let renamed = apply_key_renamers(
            self.supergraph_schema,
            type_filter_meets,
            &fetch.input_rewrites,
            vec![entry.as_read.clone()],
        );
        if same_keys(&renamed, as_sent) {
            return Ok(());
        }
        Err(ComparisonError::new(format!(
            "fetch to {}: `requires` entry {} does not agree with the fetch's input rewrites: \
             the keys it reads and the keys it sends are not the ones the rewrites carry between",
            fetch.subgraph_name, entry.item
        )))
    }
}

/// The prefixes of a flatten path that end at an element carrying a type condition, the condition
/// kept: one per `__typename` test the router makes along the path.
fn typename_inspected_paths(
    path: &[FetchDataPathElement],
) -> impl Iterator<Item = &[FetchDataPathElement]> {
    (0..path.len())
        .filter(|&end| {
            matches!(
                &path[end],
                FetchDataPathElement::Key(_, Some(_)) | FetchDataPathElement::AnyIndex(Some(_))
            )
        })
        .map(|end| &path[..=end])
}

/// The type condition each `requires` entry declares.
///
/// This is the model's `requiresWellFormed`: every entry is a type-conditioned inline fragment.
fn require_types(fetch: &FetchNode) -> Result<Vec<&Name>, ComparisonError> {
    fetch
        .requires
        .iter()
        .map(|require_item| match require_item {
            requires_selection::Selection::InlineFragment(fragment) => {
                fragment.type_condition.as_ref().ok_or_else(|| {
                    ComparisonError::new(format!(
                        "fetch to {}: a `requires` entry must carry a type condition",
                        fetch.subgraph_name
                    ))
                })
            }
            requires_selection::Selection::Field(field) => Err(ComparisonError::new(format!(
                "fetch to {}: a `requires` entry must be an inline fragment, found field `{}`",
                fetch.subgraph_name, field.name
            ))),
        })
        .collect()
}

/// The entity cases of a fetch: the type condition and selections of each `_entities` inline
/// fragment, one per entity type the fetch asks about.
///
/// Entries and cases are matched many-to-many, and the plan does not record the matching.
fn entity_cases<'a>(
    fetch: &FetchNode,
    entity_body: &'a [Selection],
) -> Result<Vec<(&'a Name, Vec<Selection>)>, ComparisonError> {
    // The router applies every fragment matching an entity, so fragments on one type are one case.
    // A fragment's own directives are kept around its selections.
    let mut cases: Vec<(&'a Name, Vec<Selection>)> = Vec::new();
    for selection in entity_body {
        let Selection::InlineFragment(fragment) = selection else {
            return Err(ComparisonError::new(format!(
                "fetch to {}: `{ENTITIES_FIELD_NAME}` must select only inline fragments",
                fetch.subgraph_name
            )));
        };
        let Some(entity_type) = &fragment.type_condition else {
            return Err(ComparisonError::new(format!(
                "fetch to {}: an `{ENTITIES_FIELD_NAME}` selection must carry a type condition",
                fetch.subgraph_name
            )));
        };
        let selections = selections::under_directives(
            &fragment.directives,
            entity_type,
            fragment.selection_set.selections.clone(),
        );
        match cases
            .iter_mut()
            .find(|(existing, _)| *existing == entity_type)
        {
            Some((_, existing)) => existing.extend(selections),
            None => cases.push((entity_type, selections)),
        }
    }
    Ok(cases)
}

//==================================================================================================
// The operations the walk builds
//==================================================================================================

impl Checker<'_> {
    /// The client operation with a different selection set, carrying its own variable
    /// declarations so the two remain comparable.
    ///
    /// Assumed valid rather than validated; see the module docs.
    fn operation_with(&self, selections: Vec<Selection>) -> Valid<ExecutableDocument> {
        // Declare only what this selection set uses: the client may declare more, and GraphQL
        // rejects an unused declaration. Only shared declarations are compared, so this is safe.
        let used = variable_usages(&selections);
        let mut built = executable::Operation {
            description: None,
            operation_type: self.operation.operation_type,
            name: self.operation.name.clone(),
            variables: self.variable_declarations(Some(&used)),
            directives: Default::default(),
            selection_set: executable::SelectionSet::new(self.root_type.clone()),
        };
        built.selection_set.selections = selections;
        let mut document = ExecutableDocument::new();
        document.operations.insert(Node::new(built));
        Valid::assume_valid(document)
    }

    /// The client operation with unfetched introspection removed; see
    /// [`without_unfetched_introspection`].
    fn without_introspection(
        &self,
        source: &Valid<ExecutableDocument>,
    ) -> Valid<ExecutableDocument> {
        let operation = self.operation;
        let mut built = operation.clone();
        built.variables = self.variable_declarations(None);
        built.selection_set.selections = without_unfetched_introspection(
            &operation.selection_set.selections,
            &source.fragments,
            true,
        );
        // The operation's fragment definitions come with it: spreads are resolved where they are
        // reached, so the definitions have to still be there to reach.
        let mut document = ExecutableDocument::new();
        document.fragments = source.fragments.clone();
        document.operations.insert(Node::new(built));
        Valid::assume_valid(document)
    }

    /// The client operation's variable declarations as every operation the checker builds them,
    /// optionally narrowed to the ones a selection set uses.
    ///
    /// Condition variables are retyped `Boolean!`; see the module docs.
    fn variable_declarations(
        &self,
        used: Option<&IndexSet<Name>>,
    ) -> Vec<Node<ast::VariableDefinition>> {
        self.operation
            .variables
            .iter()
            .filter(|variable| used.is_none_or(|used| used.contains(&variable.name)))
            .map(|variable| {
                if !self.condition_variables.contains(&variable.name) {
                    return variable.clone();
                }
                Node::new(ast::VariableDefinition {
                    description: variable.description.clone(),
                    name: variable.name.clone(),
                    ty: Node::new(ast::Type::NonNullNamed(name!("Boolean"))),
                    default_value: None,
                    directives: variable.directives.clone(),
                })
            })
            .collect()
    }
}

/// The client operation, with the introspection a query plan is not expected to fetch removed:
/// `__schema` and `__type` anywhere, and `__typename` at the operation's root only.
///
/// A field opens a new response level; an inline fragment does not, and neither does a named
/// fragment spread, which is read through at the root. Reading through happens at the use rather
/// than in the definition, so spreads of the same fragment deeper in the operation are untouched.
///
/// `__schema` and `__type` are meta-fields of the query root type, so the only place this walk can
/// miss one is inside a non-root named fragment of a schema whose query type is reachable from
/// itself.
fn without_unfetched_introspection(
    selections: &[Selection],
    fragments: &FragmentMap,
    at_root: bool,
) -> Vec<Selection> {
    selections
        .iter()
        .filter_map(|selection| match selection {
            Selection::Field(field) => {
                if is_introspection_field(&field.name)
                    || (at_root && field.name == *INTROSPECTION_TYPENAME_FIELD_NAME)
                {
                    return None;
                }
                // A field opens a new response level, so `__typename` below it is fetched like
                // any other field and only `__schema` and `__type` keep being dropped.
                let mut copy = (**field).clone();
                copy.selection_set.selections = without_unfetched_introspection(
                    &field.selection_set.selections,
                    fragments,
                    false,
                );
                if !field.selection_set.selections.is_empty()
                    && copy.selection_set.selections.is_empty()
                {
                    // Everything under it was introspection, which no fetch carries.
                    return None;
                }
                Some(Selection::Field(Node::new(copy)))
            }
            // A fragment is transparent: it does not open a new response level, so selections
            // inside one at the root are still at the root.
            Selection::InlineFragment(fragment) => {
                let mut copy = (**fragment).clone();
                copy.selection_set.selections = without_unfetched_introspection(
                    &fragment.selection_set.selections,
                    fragments,
                    at_root,
                );
                if copy.selection_set.selections.is_empty() {
                    return None;
                }
                Some(Selection::InlineFragment(Node::new(copy)))
            }
            Selection::FragmentSpread(spread) if at_root => {
                // Read as an inline fragment, which is what `query_compare` reads a spread as.
                let Some(definition) = fragments.get(&spread.fragment_name) else {
                    // Undefined: leave it for the comparison to report.
                    return Some(selection.clone());
                };
                let selections = without_unfetched_introspection(
                    &definition.selection_set.selections,
                    fragments,
                    at_root,
                );
                if selections.is_empty() {
                    return None;
                }
                let mut selection_set =
                    executable::SelectionSet::new(definition.selection_set.ty.clone());
                selection_set.selections = selections;
                Some(Selection::InlineFragment(Node::new(
                    executable::InlineFragment {
                        type_condition: Some(definition.type_condition().clone()),
                        directives: spread.directives.clone(),
                        selection_set,
                    },
                )))
            }
            Selection::FragmentSpread(_) => Some(selection.clone()),
        })
        .collect()
}

fn is_introspection_field(name: &Name) -> bool {
    name == "__schema" || name == "__type"
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every variable a selection set references, including inside list and input-object values.
fn variable_usages(selections: &[Selection]) -> IndexSet<Name> {
    let mut used = IndexSet::default();
    collect_variable_usages(selections, &mut used);
    used
}

fn collect_variable_usages(selections: &[Selection], used: &mut IndexSet<Name>) {
    for selection in selections {
        let (arguments, directives, children) = match selection {
            Selection::Field(field) => (
                Some(&field.arguments),
                &field.directives,
                &field.selection_set.selections,
            ),
            Selection::InlineFragment(fragment) => (
                None,
                &fragment.directives,
                &fragment.selection_set.selections,
            ),
            // Spreads are inlined before a fetch's selections join the buffer.
            Selection::FragmentSpread(_) => continue,
        };
        if let Some(arguments) = arguments {
            for argument in arguments {
                collect_value_variables(&argument.value, used);
            }
        }
        for directive in directives.iter() {
            for argument in &directive.arguments {
                collect_value_variables(&argument.value, used);
            }
        }
        collect_variable_usages(children, used);
    }
}

fn collect_value_variables(value: &ast::Value, used: &mut IndexSet<Name>) {
    match value {
        ast::Value::Variable(name) => {
            used.insert(name.clone());
        }
        ast::Value::List(values) => {
            for value in values {
                collect_value_variables(value, used);
            }
        }
        ast::Value::Object(fields) => {
            for (_, value) in fields {
                collect_value_variables(value, used);
            }
        }
        _ => {}
    }
}

//==================================================================================================
// Checker entry point
//==================================================================================================

/// Check that a query plan is correct for a client operation.
///
/// The schema and `operation_doc` must already be valid; neither is re-checked.
pub fn check_plan(
    supergraph_schema: &ValidFederationSchema,
    subgraphs_by_name: &IndexMap<Arc<str>, ValidFederationSchema>,
    operation_doc: &Valid<ExecutableDocument>,
    plan: &QueryPlan,
) -> Result<(), ComparisonError> {
    check_plan_with_options(
        supergraph_schema,
        subgraphs_by_name,
        operation_doc,
        plan,
        CheckerOptions::default(),
    )
}

/// [`check_plan`], with the optional checks in [`CheckerOptions`] turned on or off.
pub fn check_plan_with_options(
    supergraph_schema: &ValidFederationSchema,
    subgraphs_by_name: &IndexMap<Arc<str>, ValidFederationSchema>,
    operation_doc: &Valid<ExecutableDocument>,
    plan: &QueryPlan,
    options: CheckerOptions,
) -> Result<(), ComparisonError> {
    let operation_doc = &coerced_for_comparison(supergraph_schema, operation_doc);
    let operation = operation_doc.operations.get(None).map_err(|_| {
        ComparisonError::new("expected exactly one operation in the input document".to_string())
    })?;
    let root_type = supergraph_schema
        .schema()
        .root_operation(operation.operation_type)
        .cloned()
        .ok_or_else(|| {
            ComparisonError::new(format!(
                "schema has no {} root type",
                operation.operation_type
            ))
        })?;

    let checker = Checker::new(
        supergraph_schema,
        subgraphs_by_name,
        operation,
        plan,
        root_type.clone(),
        options,
    )?;
    let Some(node) = &plan.node else {
        // A plan with no node fetches nothing, which is correct only for an operation that asks
        // for nothing fetchable.
        return Ok(());
    };
    let walked = checker.walk_top_level(node)?;

    // Completeness: everything the plan fetches, read as one operation, must include the client
    // operation, narrowed to what the plan's fetches return.
    let plan_doc = checker.operation_with(walked.fetched);
    let operation_doc = checker.without_introspection(operation_doc);
    checker
        .comparator
        .includes_with_constraint(&checker.returned.constraint(), &plan_doc, &operation_doc)
        .map_err(|e| {
            ComparisonError::new(format!(
                "query plan does not fetch everything the operation requests:\n{e}"
            ))
        })
}
