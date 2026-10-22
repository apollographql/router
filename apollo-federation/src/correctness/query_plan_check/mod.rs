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
//! - **Soundness** — every fetch's subgraph can resolve what the fetch selects, every contextual
//!   argument the fetch passes is bound by a context rewrite reading what its declaration asks for,
//!   and every entity fetch's `requires` is matched by its entity cases, each checked where the
//!   fetch is reached, while the state that reached it is in hand. No fetch site is ever
//!   collected.
//!
//! # What a fetch must already have found
//!
//! A `requires` entry is not the authority on that; two other things are. The subgraph is sent the
//! demand, so what must already be fetched is the demand's *preimage* under the fetch's input
//! rewrites — the demand says which field each key must hold and with which arguments, which the
//! entry cannot, `trim_requires_selection_set` having dropped its arguments; the rewrites say
//! which key it is read from. The entry is checked against the two rather than read from: it has
//! the demand's shape by field name, and it agrees with the rewrites, which link each alias it
//! reads under to the field it names. A planner may fetch a `@requires` input under an alias and
//! name it back with an input key renamer, which is what the router's `execute_selection_set` and
//! `apply_rewrites` do between them.
//!
//! # Differences from the model
//!
//! - **The subgraph oracle.** Every comparison of the composed response, completeness and each
//!   requirement alike, is restricted by `SubgraphConstraint`, the legacy checker's oracle, which
//!   narrows the types a field's result can have to the ones the subgraphs declaring the field
//!   can return; it is passed to `includes_with_constraint` here, so this reaches parity with
//!   legacy rather than with the model alone, which compares the composed response unrestricted.
//! - **No fuel.** `conditionMatchesRequirementFuel` carries a budget that is an artifact of Lean's
//!   termination checker; the model's own notes say an implementation should simply loop.
//! - **What a deferred block's dependencies sent is recorded, not recomputed.** The model walks
//!   the primary block a second time, in `PrimaryDeferBlock.dependencySelections`, to find what
//!   the fetches a deferred block `depends` on sent it. Each fetch's response is what the walk
//!   already returns for it, so it is recorded under the fetch's id as the walk passes, as
//!   `FetchNode::deferred_fetches` sends it, guarded only by the condition branches inside the
//!   primary block.
//! - **Each distinct copy of a key, once.** Exact structural duplicates of a reproduced copy,
//!   source locations aside, are kept once per level, every distinct guarded contribution kept in
//!   first-occurrence order, verdicts unchanged; the model's `selectionsUnderKey` keeps them all
//!   and inclusion reads them as one. Kept, they double along a chain of fetches at one site.
//! - **`@context` data availability.** Every contextual value a fetch reads must already be
//!   fetched wherever the fetch runs. The legacy checker does not check this at all: at run time
//!   a rewrite path that reaches nothing simply drops the argument, so nothing notices.
//! - **A flatten path written to reach no type.**
//!   [`CheckerOptions::check_empty_flatten_path_type_condition`], on by default, reports a flatten
//!   path element whose condition is the empty list, `…|[]`, a fetch the planner decided can never
//!   run. The model reads that condition as it reads any condition naming no object of its
//!   position, an interface's or a union's name among them: the fetch below contributes nothing
//!   and the requirements mounted for it disappear, while its structural checks remain and, where
//!   its path ends at a key, its contextual values are still asked for, its own position being
//!   read without that key's condition. This checker decides the same with the option off. The
//!   report is this port's own and reads only whether the written list is empty, so a nonempty
//!   condition naming no object of its position is not reported.
//! - **A key's condition before an index**, a limit this port shares with the model: a condition
//!   on a key whose value is a list is read as carrying into the index, so `items|[Book, Film].@`
//!   is credited, while Router's executor compares the list itself, which has no `__typename`,
//!   and selects nothing there, a shape no planner writes.
//! - **Validity** and **condition variables** — the two sections below.
//!
//! # Validity of the operations the walk builds
//!
//! The model assumes the operations it compares are valid and carries that as a premise of its
//! correctness statement, `QueryPlanWellFormed`, naming three: the plan read as an operation, what
//! each entity fetch has already fetched, and what each `@key` demands of it, inverted through the
//! fetch's input rewrites. The checker does not decide it for any of them.
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
//! has already validated, so re-checking them here would only cost time.
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
//! A variable a condition node branches on is declared `Boolean!` in every operation the checker
//! builds, the client operation included, and loses its default. The guards synthesized for a
//! condition branch are `@skip`/`@include`, whose `if:` argument is `Boolean!` — but the branch
//! may come from `@defer(if:)`, whose argument is nullable. Executing a condition node reads the
//! value as a Boolean and the inclusion relation case-splits over true and false, so tightening
//! the declaration changes nothing either decides; it is only what makes the synthesized guard
//! well-typed. Both sides get the same treatment, so they still agree on what they share.

mod context;
mod requires;
mod resolution;
mod selections;
mod subgraph;
#[cfg(test)]
mod test_support;
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

use self::context::check_context_arguments;
use self::context::check_context_readings;
use self::context::check_context_rewrites;
use self::context::context_uses;
use self::context::context_variables;
use self::context::remove_context_arguments;
use self::requires::Reading;
use self::requires::check_requires_conflict;
use self::requires::condition_matches_requirement;
use self::requires::entry_selections;
use self::requires::inline_fragment;
use self::requires::key_half;
use self::requires::requires_half;
use self::requires::same_keys;
use self::requires::to_requires_field_set;
use self::resolution::check_fetch_resolves;
use self::selections::apply_key_renamers;
use self::selections::inline_fragment_spreads;
use self::selections::invert_key_renamers;
use self::selections::selections_at;
use self::selections::type_filter_admits;
use self::selections::type_filter_covers;
use self::selections::type_filter_meets;
use self::selections::under_condition;
use self::selections::wrap_non_empty;
use self::subgraph::KeyDirective;
use self::subgraph::Subgraph;
use super::query_compare;
use super::query_compare::conditions::BooleanLiteral;
use super::response_shape_compare::ComparisonError;
use super::subgraph_constraint::SubgraphConstraint;
use crate::correctness::CheckerOptions;
use crate::query_plan::DeferredDependency;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
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
    /// The subgraph oracle an inclusion test narrows possible types with.
    constraint: SubgraphConstraint<'a>,
    /// Every variable a condition node of this plan branches on.
    condition_variables: IndexSet<Name>,
    root_type: Name,
    /// The checks that are off unless the caller asks for them.
    options: CheckerOptions,
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
            constraint: SubgraphConstraint::new(supergraph_schema, subgraphs_by_name),
            condition_variables: condition_variables(plan),
            root_type,
            options,
        })
    }
}

/// Restricts every selection in `body` to `require_types`, the union of the type conditions this
/// fetch's `requires` entries are written at. What that union does not cover reaches nothing and
/// so contributes nothing, which is how a fetch covering only some of an interface object's
/// implementations resolves nothing for the rest.
///
/// Conditions are compared by name, not by the object types they include, which is a shortcut: a
/// `body` whose selections are all fragments on those conditions is already restricted and is
/// returned unchanged. That is the common case and the one worth recognizing, the alternative
/// leaving the rest of the checker a copy of `body` per condition to walk. Anything else is
/// restricted by adding one sibling inline fragment per condition, each carrying a copy of
/// `body`.
fn restrict_to_entry_types(require_types: &[&Name], body: Vec<Selection>) -> Vec<Selection> {
    let body_already_restricted = body.iter().all(|selection| match selection {
        Selection::InlineFragment(fragment) => fragment
            .type_condition
            .as_ref()
            .is_some_and(|type_condition| require_types.contains(&type_condition)),
        _ => false,
    });
    if body_already_restricted {
        return body;
    }
    require_types
        .iter()
        .map(|require_type| inline_fragment((*require_type).clone(), body.clone()))
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

/// A fetch data path as the checker's explanations write it, its elements joined by `.`.
fn display_path(path: &[FetchDataPathElement]) -> String {
    if path.is_empty() {
        return "the operation root".to_string();
    }
    path.iter()
        .map(|element| element.to_string())
        .collect::<Vec<_>>()
        .join(".")
}

/// Every variable a condition node of a plan branches on.
fn condition_variables(plan: &QueryPlan) -> IndexSet<Name> {
    fn walk(node: &PlanNode, out: &mut IndexSet<Name>) {
        match node {
            PlanNode::Fetch(_) => {}
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
            subscription.rest.iter().for_each(|n| walk(n, &mut out));
        }
        Some(TopLevelPlanNode::Fetch(_)) => {}
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
    /// Every fetch is checked here against its subgraph's declarations, what the subgraph can
    /// resolve and the contextual arguments it is passed, and an entity fetch against its
    /// requirements too, while the state that reached it is in hand, so no fetch site is ever
    /// collected.
    fn walk_fetch(
        &self,
        fetch: &FetchNode,
        reached: Reached<'_>,
        available: &[Selection],
        readable: &[Selection],
    ) -> Result<Vec<Selection>, ComparisonError> {
        let subgraph = self.subgraph(fetch)?;
        let operation = self.fetch_body(fetch)?;
        check_fetch_resolves(&subgraph, &fetch.subgraph_name, reached.path, &operation)?;
        let context_variables = context_variables(&fetch.context_rewrites);
        let uses = context_uses(&subgraph, &operation)?;
        check_context_arguments(fetch, reached.path, &uses, &context_variables)?;
        check_context_readings(
            self.supergraph_schema,
            &subgraph,
            &self.root_type,
            reached.path,
            available,
            fetch,
            &uses,
        )?;
        let body = operation.selections;
        // A root fetch selects its whole operation and requires nothing; an entity fetch selects
        // what its operation asks of the entities it was given, which are those of a type some
        // `requires` entry is written at: `execute_selection_set` keeps an entry's fragment only
        // for an entity of a matching type, and an entity no entry matches gets an empty
        // representation, which is never sent. So the selections are kept once per entry type,
        // under that type. For an interface object, one subgraph type standing for every
        // implementation, a fetch whose entries cover only some of them resolves nothing for the
        // rest; `interpret_fetch_node` merges every `_entities` fragment regardless.
        let selected = if fetch.requires.is_empty() {
            body
        } else {
            let entity_body = entity_body(fetch, &body)?;
            self.check_fetch(fetch, &subgraph, &entity_body, reached, available, readable)?;
            restrict_to_entry_types(&require_types(fetch)?, entity_body)
        };
        // The entity cases were read off the fetch's own operation above; what it *contributes*
        // is that selection set under the key renames its output rewrites apply and without the
        // synthetic arguments its context rewrites bind. `interpret_fetch_node` applies the two
        // in this order.
        let contributed = apply_key_renamers(
            self.supergraph_schema,
            type_filter_admits,
            &fetch.output_rewrites,
            selected,
        );
        let contributed = remove_context_arguments(&context_variables, contributed);
        Ok(selections_at(
            self.supergraph_schema,
            available,
            None,
            reached.path,
            &contributed,
        ))
    }

    /// The subgraph a fetch is sent to.
    fn subgraph(&self, fetch: &FetchNode) -> Result<Subgraph<'a>, ComparisonError> {
        let subgraph_schema = self
            .subgraphs_by_name
            .get(fetch.subgraph_name.as_ref())
            .ok_or_else(|| {
                ComparisonError::new(format!(
                    "fetch to unknown subgraph `{}`",
                    fetch.subgraph_name
                ))
            })?;
        Subgraph::new(subgraph_schema)
    }

    /// A fetch's own operation's selection set, at the operation's root type in the subgraph, its
    /// fragment spreads inlined.
    fn fetch_body(&self, fetch: &FetchNode) -> Result<executable::SelectionSet, ComparisonError> {
        let document = fetch.operation_document.as_parsed().map_err(|e| {
            ComparisonError::new(format!(
                "fetch to {}: operation document is not parsed: {e}",
                fetch.subgraph_name
            ))
        })?;
        let operation = document.operations.get(None).map_err(|_| {
            ComparisonError::new(format!(
                "fetch to {}: expected exactly one operation",
                fetch.subgraph_name
            ))
        })?;
        // Spreads go here, at the boundary where this fetch's selections join the shared buffer;
        // see `inline_fragment_spreads`.
        let mut body = executable::SelectionSet::new(operation.selection_set.ty.clone());
        body.selections =
            inline_fragment_spreads(&operation.selection_set.selections, &document.fragments)?;
        Ok(body)
    }
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

impl Checker<'_> {
    /// Whether one entity fetch is well formed, its requirements and entity cases match each
    /// other, and the contextual data it reads is already fetched.
    ///
    /// Each `requires` entry must be matched by some entity case, and each case by some entry.
    /// Legacy checks only the first direction, plus the count kept here, and does not check
    /// `@context` at all.
    ///
    /// Context rewrites are an entity fetch's business: `add_context_renamers_for_selection_set`
    /// runs on the node that has just been given its key inputs, so a fetch with them has
    /// `requires`.
    fn check_fetch(
        &self,
        fetch: &FetchNode,
        subgraph: &Subgraph<'_>,
        entity_body: &[Selection],
        reached: Reached<'_>,
        available: &[Selection],
        readable: &[Selection],
    ) -> Result<(), ComparisonError> {
        let grouped;
        let fetch = if fetch.requires.iter().any(has_conditions) {
            grouped = FetchNode {
                requires: group_requires_by_type_condition(&fetch.requires),
                ..fetch.clone()
            };
            &grouped
        } else {
            fetch
        };
        let cases = entity_cases(fetch, entity_body)?;
        let require_types = require_types(fetch)?;
        if cases.len() > require_types.len() {
            return Err(ComparisonError::new(format!(
                "fetch to {}: {} entity cases but only {} `requires` entries",
                fetch.subgraph_name,
                cases.len(),
                require_types.len()
            )));
        }

        // What the fetch can read, which every requirement is compared with. Bound once per
        // fetch, not per entry or per `@key`.
        let readable_doc = self.operation_with(readable.to_vec());

        // Only `key.fields` varies across the keys tried, so hoist the rest per case.
        let mut per_case = Vec::with_capacity(cases.len());
        for (entity_type, entity_selections) in &cases {
            let selections = requires_half(
                self.supergraph_schema,
                subgraph,
                entity_type,
                entity_selections,
            )?;
            if self.options.check_requires_conflict {
                check_requires_conflict(self.supergraph_schema, &selections).map_err(|e| {
                    ComparisonError::new(format!(
                        "fetch to {}: entity case `{entity_type}`: {}",
                        fetch.subgraph_name,
                        e.description()
                    ))
                })?;
            }
            let field_set = to_requires_field_set(&selections);
            per_case.push((subgraph.keys(entity_type)?, selections, field_set));
        }

        // An entry agrees with the fetch's input rewrites, or it is matched by no case at all:
        // the check reads the fetch and the entry, never the case or the `@key`, so it is asked
        // once per entry rather than once per pair.
        for (entry, require_type) in require_types.iter().enumerate() {
            self.check_entry_agrees_with_rewrites(fetch, entry, require_type)?;
        }

        // The table of outcomes, one per (`requires` entry, entity case) pair. Cells are filled
        // in on demand: the two checks below only ask whether *some* cell of a row, and of a
        // column, is a match, and a plan writes one entry per case it means to serve, so the
        // pairs of equal type usually settle every row and column between them. A cell is a
        // verdict and never an error, so a cell not computed cannot hide one; a row or column
        // that fails is filled in completely before it is described.
        let mut table: Table = vec![vec![None; cases.len()]; require_types.len()];
        let mut matched_entry = vec![false; require_types.len()];
        let mut matched_case = vec![false; cases.len()];
        let case_of_type: IndexMap<&Name, usize> = cases
            .iter()
            .enumerate()
            .map(|(index, case)| (case.0, index))
            .collect();

        let fill = |checker: &Self,
                    table: &mut Table,
                    entry: usize,
                    case: usize|
         -> Result<bool, ComparisonError> {
            if table[entry][case].is_none() {
                table[entry][case] = Some(checker.requirement_matches_case(
                    &fetch.requires[entry],
                    require_types[entry],
                    cases[case].0,
                    &fetch.input_rewrites,
                    &per_case[case],
                    reached,
                    available,
                    &readable_doc,
                )?);
            }
            Ok(is_match(&table[entry][case]))
        };

        // The pairs the plan meant to go together: entry `... on X` against the case `X`.
        for entry in 0..require_types.len() {
            let Some(&case) = case_of_type.get(require_types[entry]) else {
                continue;
            };
            if fill(self, &mut table, entry, case)? {
                matched_entry[entry] = true;
                matched_case[case] = true;
            }
        }

        // Every entry must be there for a reason: some case is matched by it.
        for entry in 0..require_types.len() {
            if matched_entry[entry] {
                continue;
            }
            for (case, matched) in matched_case.iter_mut().enumerate() {
                if fill(self, &mut table, entry, case)? {
                    matched_entry[entry] = true;
                    *matched = true;
                    break;
                }
            }
            if !matched_entry[entry] {
                return Err(ComparisonError::new(format!(
                    "fetch to {}: `requires` entry {} is matched by no entity case:\n{}",
                    fetch.subgraph_name,
                    fetch.requires[entry],
                    describe(computed(
                        cases.iter().map(|case| case.0),
                        table[entry].iter()
                    ))
                )));
            }
        }
        // Every case must be accounted for: some entry is matched to it.
        for case in 0..cases.len() {
            if matched_case[case] {
                continue;
            }
            for entry in 0..require_types.len() {
                if fill(self, &mut table, entry, case)? {
                    matched_case[case] = true;
                    break;
                }
            }
            if !matched_case[case] {
                return Err(ComparisonError::new(format!(
                    "fetch to {}: entity case `{}` is covered by no `requires` entry:\n{}",
                    fetch.subgraph_name,
                    cases[case].0,
                    describe(computed(
                        require_types.iter().copied(),
                        table.iter().map(|row| &row[case])
                    ))
                )));
            }
        }
        check_context_rewrites(
            self.supergraph_schema,
            &self.root_type,
            reached.path,
            reached.condition,
            available,
            readable,
            fetch,
        )
    }

    /// A `requires` entry agrees with the fetch's input rewrites, which link each alias it reads
    /// under to the field it names.
    ///
    /// The entry is read two ways — `AsRead`, each field under the key it is read from, and
    /// `AsSent`, each under its own name — and the rewrites must carry one to the other in both
    /// directions, compared key for key.
    /// - Forwards, the entry as read, renamed as `apply_rewrites` renames a representation, has
    ///   the keys it is sent under: no alias left unnamed, and no demanded field renamed away.
    /// - Inverted, the entry as sent, taken back through every rewrite, has the keys it reads
    ///   from. Since the demand's preimage is computed by that same inversion, this is what makes
    ///   it the keys *this* entry reads. It also rules out two fields sent under one key, and a
    ///   rewrite reaching a key the entry sends under its own name.
    ///
    /// A rewrite reaching nothing the entry has is allowed. That has a cost: a fetch sending two
    /// entity types where only one aliases its input, under a renamer with no type condition, is
    /// correct — the router finds nothing to rename in the other's representation — but the
    /// renamer disagrees with that entry and the plan is rejected. Scoping the renamer with a
    /// `typenameEquals` makes it agree.
    fn check_entry_agrees_with_rewrites(
        &self,
        fetch: &FetchNode,
        entry: usize,
        require_type: &Name,
    ) -> Result<(), ComparisonError> {
        let entry_item = std::slice::from_ref(&fetch.requires[entry]);
        let as_read = entry_selections(
            self.supergraph_schema,
            require_type,
            Reading::AsRead,
            entry_item,
        )?;
        let as_sent = entry_selections(
            self.supergraph_schema,
            require_type,
            Reading::AsSent,
            entry_item,
        )?;
        let renamed = apply_key_renamers(
            self.supergraph_schema,
            type_filter_meets,
            &fetch.input_rewrites,
            as_read.clone(),
        );
        let inverted = invert_key_renamers(
            self.supergraph_schema,
            type_filter_covers,
            &fetch.input_rewrites,
            as_sent.clone(),
        );
        if same_keys(&renamed, &as_sent) && same_keys(&inverted, &as_read) {
            return Ok(());
        }
        Err(ComparisonError::new(format!(
            "fetch to {}: `requires` entry {} does not agree with the fetch's input rewrites: \
             the keys it reads and the keys it sends are not the ones the rewrites carry between",
            fetch.subgraph_name, fetch.requires[entry]
        )))
    }

    /// Whether one `requires` entry is matched by one entity case: the subgraph resolves that
    /// case from a key the plan has already fetched, and the entry declares exactly that key.
    ///
    /// `None` means matched; `Some(why)` says why not, per `@key` tried. An `Err` is a malformed
    /// plan or schema, not a mismatch.
    ///
    /// The `@key`-does-not-apply arm below has no test; `tests.rs` does not reach it. It needs two things at once: entity types whose keys name different
    /// fields, so that one type's key can fail to typecheck at another's entry, and a plan whose
    /// pairs of equal type do not settle the table, so that the search looks at any other pair at
    /// all. A plan the planner produced never supplies the second. Covering it means writing a
    /// plan whose diagonal fails by hand.
    #[allow(clippy::too_many_arguments)]
    fn requirement_matches_case(
        &self,
        require_item: &requires_selection::Selection,
        require_type: &Name,
        entity_type: &Name,
        input_rewrites: &[Arc<FetchDataRewrite>],
        hoisted: &(
            Vec<KeyDirective>,
            Vec<Selection>,
            Vec<requires_selection::Selection>,
        ),
        reached: Reached<'_>,
        available: &[Selection],
        readable_doc: &Valid<ExecutableDocument>,
    ) -> Result<Option<String>, ComparisonError> {
        let (keys, requires_selections, requires_field_set) = hoisted;
        if keys.is_empty() {
            return Ok(Some(format!(
                "  the subgraph declares no `@key` on `{entity_type}`"
            )));
        }
        let mut unmatched = Vec::new();
        for key in keys {
            if !key.resolvable {
                unmatched.push(format!(
                    "  @key({}): the subgraph will not resolve the entity from this key",
                    key.fields
                ));
                continue;
            }
            // `entityFetchRequirement`, in its two halves: the conversion to a field set
            // distributes over concatenation, so the halves can be read separately.
            let key_selections = match key_half(self.supergraph_schema, require_type, &key.fields) {
                Ok(selections) => selections,
                // A key that does not typecheck here is this pair saying no; see `key_half`.
                Err(reason) => {
                    unmatched.push(format!(
                        "  @key({}): does not apply to `{require_type}`:\n{}",
                        key.fields,
                        indent(&reason.to_string())
                    ));
                    continue;
                }
            };
            let mut demanded = to_requires_field_set(&key_selections);
            demanded.extend(requires_field_set.iter().cloned());
            if let Err(mismatch) = condition_matches_requirement(
                self.supergraph_schema,
                std::slice::from_ref(&require_item),
                &demanded.iter().collect::<Vec<_>>(),
            ) {
                unmatched.push(format!(
                    "  @key({}): the entry does not declare what the subgraph demands:\n{}",
                    key.fields,
                    indent(&mismatch.to_string())
                ));
                continue;
            }
            // The subgraph is sent the demand, so what must already be fetched is the demand's
            // preimage under the fetch's input rewrites: each one inverted, where its filter
            // covers the position.
            //
            // Asked only at the entry's own type. The router builds this entry's representation
            // only for entities of that type, and the demand's `@requires` half sits at the
            // subgraph's entity type, which for an interface object reaches every implementation;
            // another implementation's inputs are its own entry's business. Restricting first is
            // also what lets a renamer be inverted at all: `type_filter_covers` asks whether a
            // filter admits every type a position can hold, which a position nothing is known
            // about never satisfies.
            //
            // Mounted where the fetch runs and guarded by the condition it runs under, so the
            // comparison happens at the query root.
            let mut computed = key_selections;
            computed.extend(requires_selections.iter().cloned());
            let computed = invert_key_renamers(
                self.supergraph_schema,
                type_filter_covers,
                input_rewrites,
                wrap_non_empty(
                    |selections| inline_fragment(require_type.clone(), selections),
                    computed,
                ),
            );
            let required = under_condition(
                reached.condition,
                &self.root_type,
                selections_at(
                    self.supergraph_schema,
                    available,
                    None,
                    reached.path,
                    &computed,
                ),
            );
            let required_doc = self.operation_with(required);
            // Mounted into what was fetched before the fetch, compared with what it can read: a
            // deferred block that cannot read a requirement there is rejected rather than having
            // the requirement vanish from it.
            if let Err(e) = self.comparator.includes_with_constraint(
                &self.constraint,
                readable_doc,
                &required_doc,
            ) {
                unmatched.push(format!(
                    "  @key({}): the plan has not fetched what the subgraph demands:\n{}",
                    key.fields,
                    indent(&e.to_string())
                ));
                continue;
            }
            return Ok(None);
        }
        Ok(Some(unmatched.join("\n")))
    }
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

fn has_conditions(item: &requires_selection::Selection) -> bool {
    matches!(item, requires_selection::Selection::InlineFragment(inline) if !inline.conditions.is_empty())
}

/// The router builds one representation per entity from every top-level `requires` item whose
/// type condition matches it, so items sharing a type condition are one entry. A conditional item
/// is nested under its group so its conditions are kept.
fn group_requires_by_type_condition(
    requires: &[requires_selection::Selection],
) -> Vec<requires_selection::Selection> {
    let mut grouped: Vec<requires_selection::Selection> = Vec::new();
    for item in requires {
        let requires_selection::Selection::InlineFragment(inline) = item else {
            grouped.push(item.clone());
            continue;
        };
        let group = grouped.iter_mut().find_map(|existing| match existing {
            requires_selection::Selection::InlineFragment(existing)
                if existing.type_condition == inline.type_condition =>
            {
                Some(existing)
            }
            _ => None,
        });
        match group {
            None if inline.conditions.is_empty() => grouped.push(item.clone()),
            None => grouped.push(requires_selection::Selection::InlineFragment(
                requires_selection::InlineFragment {
                    type_condition: inline.type_condition.clone(),
                    conditions: Vec::new(),
                    selections: vec![item.clone()],
                },
            )),
            Some(group) if inline.conditions.is_empty() => {
                group.selections.extend(inline.selections.iter().cloned())
            }
            Some(group) => group.selections.push(item.clone()),
        }
    }
    grouped
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

/// Renders the outcomes of one row or column of the requirement table.
/// An outcome that has been computed: `None` is a match, `Some(why)` says why not.
type Cell = Option<Option<String>>;
type Table = Vec<Vec<Cell>>;

/// Whether a computed cell is a match. An uncomputed cell is not one.
fn is_match(cell: &Cell) -> bool {
    matches!(cell, Some(None))
}

/// A row or column of the outcome table paired with its names, skipping cells never computed.
/// Pairing before skipping is what keeps a name with its own outcome; the failing row or column
/// is filled in by the time it is described, so nothing is dropped in practice.
fn computed<'a>(
    names: impl Iterator<Item = &'a Name>,
    cells: impl Iterator<Item = &'a Cell>,
) -> impl Iterator<Item = (&'a Name, &'a Option<String>)> {
    names
        .zip(cells)
        .filter_map(|(name, cell)| cell.as_ref().map(|outcome| (name, outcome)))
}

fn describe<'a>(outcomes: impl Iterator<Item = (&'a Name, &'a Option<String>)>) -> String {
    outcomes
        .map(|(name, outcome)| match outcome {
            Some(why) => format!("* `{name}`:\n{why}"),
            None => format!("* `{name}`: matched"),
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    // operation. `SubgraphConstraint` restricts this comparison of the composed response, which the
    // model makes unrestricted: the remaining difference § Differences from the model names.
    let plan_doc = checker.operation_with(walked.fetched);
    let operation_doc = checker.without_introspection(operation_doc);
    checker
        .comparator
        .includes_with_constraint(&checker.constraint, &plan_doc, &operation_doc)
        .map_err(|e| {
            ComparisonError::new(format!(
                "query plan does not fetch everything the operation requests:\n{e}"
            ))
        })
}
