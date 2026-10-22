//! Query plan correctness, ported from the Lean `checkQueryPlan` model.
//!
//! Port of `Apollo/Implementations/QueryPlanChecker.lean` in `apollo-graphql-lean`, where
//! `CheckQueryPlanCorrect` states that the checker accepts exactly the plans that are complete and
//! sound, under the premises below, and `checkQueryPlan_correct` proves it.
//!
//! The check walks the plan once, building what it fetches as the copies fetches write and the
//! contributions on them ([`response`], where a copy and a contribution are defined), and checks
//! each fetch where it is reached, while the state that reached it is in hand. No fetch site is
//! ever collected.
//!
//! - **Completeness** — the client operation is covered by that response: every field it asks for
//!   is fetched on every copy it can be asked at, whatever runtime types the copies have, which
//!   [`coverage::Coverage::covers`] decides. A copy is the value one response key holds at one
//!   parent copy, and every fetch writing that key there writes the same value, so their
//!   contributions combine; a different parent, alias or list holds a different copy, owed its own
//!   fields. What a copy can be is read from every contribution to it: each declares the possible
//!   runtime types of its result in its own subgraph, at the subgraph type it was selected on, each
//!   standing for a supergraph object, and the copy can be what every active contribution allows
//!   (`hypothesis.rs`). A list one subgraph declares over fewer members than the supergraph holds
//!   returns only those members from that subgraph, and a field with no possible object result
//!   types owes nothing below it, while the field itself is still owed.
//! - **Soundness** — every fetch's subgraph can resolve what the fetch selects, the type conditions
//!   on the path it runs at read a fetched `__typename` value, every contextual argument the fetch
//!   passes is bound by a context rewrite reading what its declaration asks for, and an entity
//!   fetch's representation, what its `requires` entries project where it runs, is read from what
//!   the plan fetched and covers what the subgraph demands, in each configuration, as the
//!   contextual values it reads were fetched.
//! - **The Boolean boundary** — every Boolean variable the plan's conditions read is one the
//!   operation declares a `Boolean`, which is what the configurations are made of
//!   ([`check_boolean_variables`]).
//!
//! # What a fetch must already have found
//!
//! The representation an entity fetch sends is what the router's `execute_selection_set` projects
//! from its `requires` entries at each object it runs on, the entries' type conditions and
//! `@skip`/`@include` conditions read there, fields replacing what an earlier entry left under
//! their response key and fragments merging (`representation.rs`). Everything it reads must have
//! been fetched, under the field the entry names, a value read and later overwritten included.
//! What it must hold is the subgraph's demand, not the entries': the subgraph is sent the demand,
//! so what must already be fetched is the demand's *preimage* under the fetch's input rewrites —
//! the demand says which field each key must hold and with which arguments, which an entry cannot,
//! `trim_requires_selection_set` having dropped its arguments; the rewrites say which key it is
//! read from. The representation covers that preimage, through some resolvable key in each
//! configuration, and may hold more; what it explicitly selects, and every demanded field it holds,
//! agrees with the rewrites, which link each alias it is read under to the field it names. A
//! planner may fetch a `@requires` input under an alias and name it back with an input key
//! renamer, which is what the router's `execute_selection_set` and `apply_rewrites` do between
//! them.
//!
//! # What the checker assumes of the supergraph and the subgraphs
//!
//! Reading what a copy can be from the subgraphs contributing to it rests on the *declaration
//! correspondence*: each subgraph's declarations correspond to the supergraph's, every object a
//! subgraph declares standing for supergraph objects, which the model states as
//! `Subgraph.DeclarationCorrespondence` and takes as a premise. Composition establishes it for what
//! composition checks: the `join__type`, `join__implements` and `join__unionMember` annotations at
//! the consumed join version tie each subgraph's types, implementations and members to the
//! supergraph's, an absent annotation proving no exclusion; an `@interfaceObject` names a
//! supergraph interface and stands for each of its objects; and `merge_interface.rs` rejects an
//! interface object beside an implementation of its interface in one subgraph, and two interface
//! objects in one subgraph whose interfaces share an object. The checker checks none of it: it
//! reads the supergraph it is given, and `Supergraph::new_with_router_specs` validates that schema
//! and its specs and reruns no composition.
//!
//! One assumption goes further than any composition rule is known to: that every subgraph-declared
//! result of a field of an interface object fits the corresponding field at every supergraph object
//! the interface object may represent. It is unestablished. The checker relies on it wherever it
//! grounds an interface object's results, and checks it nowhere; what would discharge it is a
//! composition rule enforcing it, or a survey over the corpus showing every interface object's
//! fields to satisfy it.
//!
//! Reading it from subgraphs the plan does not ask rests on *sharing conformance*: the subgraphs
//! resolving one field at one supergraph parent, with the same arguments, return the same value, so
//! each of their declarations bounds what the value can be whichever subgraph is asked
//! (`sharing.rs`); the two declarations of a progressive override, which resolve the field under
//! opposite conditions on its label, are alternatives rather than interchangeable. Apollo's resolver-consistency guidance asks it of every shared field, and
//! composition's `shareable_runtime_types::hints_for_interfaces` reads a shared interface field's
//! result as the intersection of its declarations; composition does not verify that deployed
//! resolvers agree. It supplies no field and no `__typename` value, only a bound on runtime types.
//! The same premise is what lets repeated writes of one key at one parent combine: the router's
//! `type_aware_deep_merge` merges objects field by field and lists index by index, and two fetches
//! writing one list are read as writing the same elements in the same order.
//!
//! What the checker does not assume, nor check, is that resolvers obey their declarations and
//! return the entity they were given, an interface object's particular supergraph object included:
//! that is source conformance, an assumption about execution.
//!
//! # Differences from the model
//!
//! - **No fuel.** The model's coverage and its rewrite agreement over a representation carry
//!   budgets that are artifacts of Lean's termination checker; its own notes say an
//!   implementation should simply loop.
//! - **A representation is regions, not a value per configuration.** The model projects the
//!   entries in every configuration; here the projection is read once, as the configurations in
//!   which each location of the representation is present, explicitly selected and read, which
//!   the model's projection determines because every write at one location reads the same
//!   contributions (`representation.rs`). A key is still chosen per configuration: where no one
//!   key covers every configuration, the configurations each key fails in are intersected.
//! - **An entity fetch's additions are not withheld where it sends nothing.** The model's
//!   `FetchNode.producedResponse` credits an entity fetch only where its representation is
//!   non-empty. Wherever this checker lets the fetch run and its representation is empty, the
//!   fetch must select nothing active there or it is refused, so what the model withholds is
//!   inactive there and the verdicts are the same.
//! - **Configurations are decided, not enumerated.** The model asks each obligation under every
//!   Boolean assignment and every configuration of the copies' runtime types. Here a copy's
//!   runtime type is a variable and every obligation is a region of configurations to be covered
//!   by others, decided over the variables it mentions (`region.rs`, `hypothesis.rs`); a
//!   configuration the model would enumerate is never built.
//! - **What a deferred block's dependencies sent is recorded, not recomputed.** The model walks
//!   the primary block a second time, in `PrimaryDeferBlock.dependencyResponse`, to find what the
//!   fetches a deferred block `depends` on sent it. Each fetch's additions are what the walk
//!   already returns for it, so they are recorded under the fetch's id as the walk passes, with the
//!   contributions above them that lead to them, the model's `withResponseAncestors`, as
//!   `FetchNode::deferred_fetches` sends them.
//! - **Each distinct contribution of a fetch, once.** A contribution identical to one the same
//!   fetch has already made on the same copy, in everything the model identifies one by — copy,
//!   key, subgraph parent type, field, arguments and guard — is kept once, and what is selected
//!   under the duplicate is still produced; verdicts are unchanged. The model keeps them all.
//! - **`@context` data availability.** Every contextual value a fetch reads must already be
//!   fetched wherever the fetch runs. The legacy checker does not check this at all: at run time
//!   a rewrite path that reaches nothing simply drops the argument, so nothing notices.
//! - **A flatten path written to reach no type.**
//!   [`CheckerOptions::check_empty_flatten_path_type_condition`], on by default, reports a flatten
//!   path element whose condition is the empty list, `…|[]`, a fetch the planner decided can never
//!   run. The model reads that condition as it reads any condition naming no object of its
//!   position, an interface's or a union's name among them: the path reaches no copy, so the
//!   fetch runs nowhere, owes nothing there and adds nothing, while its structural checks remain.
//!   This checker decides the same with the option off. The report is this port's own and reads
//!   only whether the written list is empty, so a nonempty condition naming no object of its
//!   position is not reported.
//! - **A key's condition before an index**, a limit this port shares with the model: a condition
//!   on a key whose value is a list is read as carrying into the index, so `items|[Book, Film].@`
//!   is credited, while Router's executor compares the list itself, which has no `__typename`,
//!   and selects nothing there, a shape no planner writes.
//! - **Validity** — the section below.
//!
//! # Validity of the operations the checker reads
//!
//! The model assumes the client operation and every fetch's own operation are valid, and carries
//! that in its correctness statement (`QueryPlanWellFormed` and its premise on the operation); the
//! checker decides neither. Nothing else is assumed valid, since the checker builds no operation:
//! a fetch's selections are read into the response field by field, on the copies they reach, and
//! what a requirement demands is read against the response as a demand, never as an operation.
//!
//! Where the fields a demand selects under one key at one supergraph type make one call — the
//! client operation's validity gives it, and a requirement assembled from one entity's field sets
//! has it unless two `@requires` demand one key differently, which
//! [`CheckerOptions::check_requires_conflict`] reports — every fetched field that can be active
//! with one of them has to make it. Where they make several, the model's reading, some active
//! demanded call made by every active fetched field, is asked assignment by assignment over the
//! key's own guards.
//!
//! # Unfetched introspection
//!
//! The client operation is compared with some introspection removed, mirroring the legacy
//! checker's policy. The planner ignores `__schema` and `__type`, so no plan ever fetches them.
//! The router answers a *root* `__typename` without asking a subgraph, so no fetch carries it;
//! below the root it is fetched like any other field and is owed. This is query-plan policy
//! rather than part of the coverage relation, which is why it lives here and not in [`response`].

mod context;
mod coverage;
mod hypothesis;
mod region;
mod representation;
mod requires;
mod resolution;
mod response;
mod selections;
mod sharing;
mod subgraph;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexMap;
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
use self::coverage::Coverage;
use self::coverage::Demand;
use self::coverage::Supplier;
use self::coverage::Supplies;
use self::coverage::render_clause;
use self::hypothesis::Hypothesis;
use self::region::Algebra;
use self::region::Clause;
use self::region::Work;
use self::region::boolean;
use self::region::everything;
use self::representation::Agreement;
use self::representation::Projection;
use self::representation::check_explicit_agreement;
use self::representation::check_inputs_available;
use self::representation::entity_fragments;
use self::representation::requires_projection;
use self::representation::requires_variables;
use self::resolution::check_fetch_resolves;
use self::response::ContributionId;
use self::response::CopyId;
use self::response::Mount;
use self::response::Position;
use self::response::Production;
use self::response::Response;
use self::response::View;
use self::response::every_object;
use self::response::union;
use self::selections::inline_fragment_spreads;
use self::sharing::Sharing;
use self::subgraph::Subgraph;
use super::query_compare::conditions::BooleanLiteral;
use super::query_compare::schema_view::SchemaView;
use super::response_shape_compare::ComparisonError;
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

/// The state a fetch is reached with.
#[derive(Clone, Copy)]
struct Reached<'a> {
    /// Where the fetch runs, as a path of response keys from the query root.
    path: &'a [FetchDataPathElement],
    /// The condition-node branches in effect, read as a conjunction.
    condition: &'a [BooleanLiteral],
    /// Where the fetch records what it sends, when it runs in the primary block of a `defer` node
    /// whose deferred blocks may name it as a dependency.
    deferred: Option<&'a DeferredFetches>,
}

/// Where a fetch's soundness is decided: the model's `FetchSite`, with the copy it runs on found
/// once.
struct Site<'s> {
    fetch: &'s FetchNode,
    /// Where the fetch runs, as a path of response keys from the query root.
    path: &'s [FetchDataPathElement],
    schema: &'s SchemaView<'s>,
    /// The copy the path addresses.
    position: &'s Position,
    /// The configurations the fetch runs there in: its path's type conditions, the condition-node
    /// branches it runs under and, for an entity fetch, the runtime types some `requires` entry
    /// admits.
    region: Clause,
    /// The admissible configurations of what was fetched before the fetch, where its position and
    /// the copies it reads are.
    hypothesis: &'s Hypothesis<'s>,
    /// What the fetch can read, where what it is sent has to be.
    readable: View<'s>,
    /// What the fetch can read, by copy and key, for the walks over it.
    supplies: Supplies<'s>,
}

/// What each fetch in a `defer` node's primary block sends the deferred blocks that name it.
///
/// Recorded as the walk passes each fetch, since `FetchNode::deferred_fetches` sends a fetch's own
/// response rather than everything fetched before it. A nested `defer` node keeps its own, so no
/// fetch inside one is recorded here.
struct DeferredFetches {
    /// Each fetch's id and what it sends: its additions, with the contributions above them that
    /// lead to them and none of those copies' other values. In the order the walk passed them.
    responses: RefCell<Vec<(u64, Vec<ContributionId>)>>,
}

impl DeferredFetches {
    fn new() -> Self {
        DeferredFetches {
            responses: Default::default(),
        }
    }

    /// What the fetches a deferred block depends on sent it. `depends` holds fetch ids as strings.
    fn responses_of(&self, depends: &[DeferredDependency]) -> Vec<ContributionId> {
        let mut sent = Vec::new();
        for (id, response) in self.responses.borrow().iter() {
            let id = id.to_string();
            if depends.iter().any(|dependency| dependency.id == id) {
                sent = union(&sent, response);
            }
        }
        sent
    }
}

pub(crate) struct Checker<'a> {
    supergraph_schema: &'a ValidFederationSchema,
    /// The supergraph's possible types, indexed once: every supergraph type condition, a demand's
    /// or a `requires` entry's, and every supergraph object a subgraph object stands for, is read
    /// here.
    schema_view: SchemaView<'a>,
    /// Read for each fetch's declarations and the cases of what it returns.
    subgraphs_by_name: &'a IndexMap<Arc<str>, ValidFederationSchema>,
    /// Every subgraph's declarations, for the sharing clause.
    sharing: Sharing<'a>,
    /// The supergraph's objects, the sets of them literals hold, and the work counters.
    algebra: Algebra,
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
        options: CheckerOptions,
    ) -> Result<Self, ComparisonError> {
        let schema_view = SchemaView::new(supergraph_schema)
            .map_err(|e| ComparisonError::new(format!("failed to index the supergraph: {e}")))?;
        let sharing = Sharing::new(subgraphs_by_name)?;
        Ok(Checker {
            supergraph_schema,
            schema_view,
            subgraphs_by_name,
            sharing,
            algebra: Algebra::new(supergraph_schema),
            options,
        })
    }
}

/// Does every type condition on the path a fetch runs at read a fetched `__typename` value? The
/// model's `FetchSitePathObserved`.
///
/// A conditioned key or index is followed only into copies whose `__typename` value names a type
/// the condition names: the router's `iterate_path` reads it there, and finds nothing where it was
/// not fetched. The position was found without reading those values, so a missing one does not
/// erase the position it is owed at; it has to be readable there wherever the fetch runs at that
/// position, under the guards it was fetched under.
fn check_path_observed(site: &Site<'_>) -> Result<(), ComparisonError> {
    let hypothesis = site.hypothesis;
    let response = site.readable.response;
    for read in &site.position.reads {
        let supplies = hypothesis.supplies(site.readable.typename_guards(*read));
        if let Some(example) = hypothesis.uncovered(site.position.copy, &site.region, &supplies) {
            return Err(ComparisonError::new(format!(
                "fetch to {}: the path `{}` reads the `__typename` of {}, which the plan has \
                 not fetched there{}",
                site.fetch.subgraph_name,
                display_path(site.path),
                response.describe(*read),
                render_clause(hypothesis.algebra, response, &example),
            )));
        }
    }
    Ok(())
}

/// Reports a flatten path element narrowed to no runtime type at all.
///
/// The planner writes an empty type condition — `…@|[]` — when it has worked out that the
/// position admits nothing, which says the fetch under the path can never run. The judgement is
/// right; emitting the fetch anyway is not. The rest of this checker is silent about it, and
/// correctly so: a path element that reaches no copy leaves the fetch no position to run at, so
/// it owes nothing and adds nothing. That makes it dead code in the plan rather than a soundness
/// violation, which is why [`CheckerOptions::check_empty_flatten_path_type_condition`] can turn it
/// off.
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

//==================================================================================================
// Walking node tree
//==================================================================================================

impl<'a> Checker<'a> {
    /// Walks a plan node once, returning the fields it adds to the response.
    ///
    /// Each fetch is checked where it is reached, while the state that reached it is in hand, so
    /// no fetch site is ever collected. The threading is the model's: a sequence's children run in
    /// order, parallel children all see the same input, a defer node's deferred blocks start after
    /// its primary block.
    ///
    /// Two views of the response are threaded. `available` is everything fetched before the node,
    /// which locates where a fetch runs and what each boundary can hold; `readable` is what of that
    /// the node can read, which supplies the values a fetch is sent. They differ only inside a
    /// deferred block. Both are sorted lists of contribution ids into `response`.
    fn walk_node(
        &self,
        response: &mut Response,
        node: &PlanNode,
        reached: Reached<'_>,
        available: &[ContributionId],
        readable: &[ContributionId],
    ) -> Result<Vec<ContributionId>, ComparisonError> {
        match node {
            PlanNode::Fetch(fetch) => {
                let added = self.walk_fetch(response, fetch, reached, available, readable)?;
                if let (Some(deferred), Some(id)) = (reached.deferred, fetch.id) {
                    let sent = response.sent(&self.algebra, available, &added);
                    deferred.responses.borrow_mut().push((id, sent));
                }
                Ok(added)
            }
            PlanNode::Sequence(sequence) => {
                // Each child sees, and can read, what the previous ones added.
                let mut added: Vec<ContributionId> = Vec::new();
                for child in &sequence.nodes {
                    let seen = union(available, &added);
                    let seen_readable = union(readable, &added);
                    let child_added =
                        self.walk_node(response, child, reached, &seen, &seen_readable)?;
                    added = union(&added, &child_added);
                }
                Ok(added)
            }
            PlanNode::Parallel(parallel) => {
                // Every child sees the same input.
                let mut added: Vec<ContributionId> = Vec::new();
                for child in &parallel.nodes {
                    let child_added =
                        self.walk_node(response, child, reached, available, readable)?;
                    added = union(&added, &child_added);
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
                    response,
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
                let deferred_fetches = DeferredFetches::new();
                let primary = match &defer.primary.node {
                    Some(node) => self.walk_node(
                        response,
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
                // and what the fetches it depends on send it — or the whole primary block when it
                // names none, `DeferredNode::execute` then waiting for all of it.
                let block_available = union(available, &primary);
                let mut added = primary.clone();
                for block in &defer.deferred {
                    let Some(node) = &block.node else {
                        continue;
                    };
                    let block_readable = if block.depends.is_empty() {
                        union(readable, &primary)
                    } else {
                        union(readable, &deferred_fetches.responses_of(&block.depends))
                    };
                    let block_added = self.walk_node(
                        response,
                        node,
                        Reached {
                            deferred: None,
                            ..reached
                        },
                        &block_available,
                        &block_readable,
                    )?;
                    added = union(&added, &block_added);
                }
                Ok(added)
            }
            PlanNode::Condition(node) => {
                // Each branch's fields carry the branch's literal in their guards.
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
                    condition.push(literal);
                    let branch_added = self.walk_node(
                        response,
                        clause,
                        Reached {
                            condition: &condition,
                            ..reached
                        },
                        available,
                        readable,
                    )?;
                    added = union(&added, &branch_added);
                }
                Ok(added)
            }
        }
    }

    /// Walks a root plan node, returning everything the plan fetches. A subscription plan is read
    /// as a query, as `interpret_subscription_node` reads it.
    fn walk_top_level(
        &self,
        response: &mut Response,
        node: &TopLevelPlanNode,
    ) -> Result<Vec<ContributionId>, ComparisonError> {
        let reached = Reached {
            path: &[],
            condition: &[],
            deferred: None,
        };
        match node {
            TopLevelPlanNode::Subscription(subscription) => {
                // The primary fetch opens the stream, so it precedes the rest of the plan.
                let primary =
                    self.walk_fetch(response, &subscription.primary, reached, &[], &[])?;
                let mut added = primary.clone();
                if let Some(rest) = &subscription.rest {
                    let rest_added = self.walk_node(response, rest, reached, &primary, &primary)?;
                    added = union(&added, &rest_added);
                }
                Ok(added)
            }
            TopLevelPlanNode::Fetch(fetch) => {
                self.walk_node(response, &PlanNode::Fetch(fetch.clone()), reached, &[], &[])
            }
            TopLevelPlanNode::Sequence(node) => self.walk_node(
                response,
                &PlanNode::Sequence(node.clone()),
                reached,
                &[],
                &[],
            ),
            TopLevelPlanNode::Parallel(node) => self.walk_node(
                response,
                &PlanNode::Parallel(node.clone()),
                reached,
                &[],
                &[],
            ),
            TopLevelPlanNode::Flatten(node) => self.walk_node(
                response,
                &PlanNode::Flatten(node.clone()),
                reached,
                &[],
                &[],
            ),
            TopLevelPlanNode::Defer(node) => {
                self.walk_node(response, &PlanNode::Defer(node.clone()), reached, &[], &[])
            }
            TopLevelPlanNode::Condition(node) => self.walk_node(
                response,
                &PlanNode::Condition(node.clone()),
                reached,
                &[],
                &[],
            ),
        }
    }

    /// What one fetch adds to the response, on the copy it runs on.
    ///
    /// Every fetch is checked here against its subgraph's declarations, what the subgraph can
    /// resolve and the contextual arguments it is passed, the `__typename` values its path reads
    /// and, for an entity fetch, its requirements and contextual values, while the state that
    /// reached it is in hand, so no fetch site is ever collected.
    fn walk_fetch(
        &self,
        response: &mut Response,
        fetch: &FetchNode,
        reached: Reached<'_>,
        available: &[ContributionId],
        readable: &[ContributionId],
    ) -> Result<Vec<ContributionId>, ComparisonError> {
        let subgraph = self.subgraph(fetch)?;
        let operation = self.fetch_body(fetch)?;
        check_fetch_resolves(&subgraph, &fetch.subgraph_name, reached.path, &operation)?;
        let context_variables = context_variables(&fetch.context_rewrites);
        let uses = context_uses(&subgraph, &operation)?;
        check_context_arguments(fetch, reached.path, &uses, &context_variables)?;
        // A root fetch selects its whole operation; an entity fetch selects what its operation asks
        // of the entities it was given, read at the subgraph type each one is in the subgraph.
        let entity_selections = if fetch.requires.is_empty() {
            None
        } else {
            Some(entity_body(fetch, &operation.selections)?)
        };
        let algebra = &self.algebra;

        // Where the fetch runs, which everything asked of it there is judged over, found once.
        let Some(position) = response.position(algebra, reached.path) else {
            return Ok(Vec::new());
        };
        let Some(region) = self.site_region(response, &position, reached.condition, fetch) else {
            return Ok(Vec::new());
        };
        // The subgraph entity types an entity fetch's `_entities` can return, each with the
        // supergraph objects it stands for.
        let mut entity_types: Vec<(Name, Vec<Name>)> = Vec::new();
        if entity_selections.is_some() {
            for (subgraph_type, object) in subgraph
                .field_runtime_types(
                    &algebra.work,
                    &self.schema_view,
                    &operation.ty,
                    &ENTITIES_FIELD_NAME,
                )
                .into_iter()
                .flatten()
            {
                match entity_types
                    .iter_mut()
                    .find(|(known, _)| *known == subgraph_type)
                {
                    Some((_, objects)) => objects.push(object),
                    None => entity_types.push((subgraph_type, vec![object])),
                }
            }
        }
        // What an entity fetch sends where it runs: its representation, held as contributions of
        // its own that no other view lists.
        let projection = entity_selections.as_ref().map(|_| {
            requires_projection(
                algebra,
                &self.schema_view,
                response.view(readable),
                position.copy,
                &region,
                &fetch.requires,
            )
        });
        let held = match &projection {
            Some(projection) => {
                let held = projection.held(algebra, response.view(readable));
                response.detached(held)
            }
            None => Vec::new(),
        };
        let base = {
            let available_view = response.view(available);
            let hypothesis = Hypothesis::new(algebra, available_view);
            let site = Site {
                fetch,
                path: reached.path,
                schema: &self.schema_view,
                position: &position,
                region: region.clone(),
                hypothesis: &hypothesis,
                readable: response.view(readable),
                supplies: Supplies::new(response.view(readable)),
            };
            check_context_readings(&site, &subgraph, &uses)?;
            check_path_observed(&site)?;
            if let (Some(entity_selections), Some(projection)) = (&entity_selections, &projection) {
                self.check_entity_fetch(
                    fetch,
                    &subgraph,
                    entity_selections,
                    &entity_types,
                    projection,
                    response.view(&held),
                    &site,
                )?;
                // Context rewrites are an entity fetch's business:
                // `add_context_renamers_for_selection_set` runs on the node that has just been
                // given its key inputs, and the router resolves none for a fetch that requires
                // nothing.
                check_context_rewrites(&site)?;
            }
            // The fetch adds where what was fetched before it reaches its position, and the
            // `__typename` values its path reads were fetched, under their guards.
            let domain = every_object(algebra);
            let mut base = algebra.product(
                &available_view.reach(algebra, &position.steps),
                std::slice::from_ref(&region),
            );
            for read in &position.reads {
                base = algebra.simplify(
                    algebra.product(&base, &available_view.typename_guards(*read)),
                    &domain,
                );
            }
            algebra.simplify(base, &domain)
        };
        if base.is_empty() {
            return Ok(Vec::new());
        }

        // The subgraph types the fetch's operation is read at: the root type, or each possible
        // runtime type of `_entities`, with the supergraph objects it stands for. A root type
        // stands for the supergraph's root type of the fetch's own operation kind, on whatever
        // copy its path reaches: a query fetch flattened at a `Query` a mutation's field returns
        // runs on that `Query`, and at an empty path the copy is the response's root, which only
        // a fetch of the response's own kind stands for. An interface object stands for each of
        // its interface's objects, and the copy it contributes to keeps the entity's supergraph
        // runtime type.
        let cases = if fetch.requires.is_empty() {
            let Some(root) = self
                .supergraph_schema
                .schema()
                .root_operation(fetch.operation_kind)
            else {
                return Err(ComparisonError::new(format!(
                    "fetch to {}: the supergraph has no {} root type",
                    fetch.subgraph_name, fetch.operation_kind
                )));
            };
            vec![(operation.ty.clone(), algebra.objects([root]))]
        } else {
            entity_types
                .into_iter()
                .map(|(subgraph_type, objects)| (subgraph_type, algebra.objects(&objects)))
                .collect()
        };
        // What the fetch adds is its operation without the synthetic arguments its context
        // rewrites bind, under the key renames its output rewrites apply. `interpret_fetch_node`
        // applies the two in this order.
        let producer = response.new_producer(fetch.subgraph_name.clone());
        let production = Production {
            subgraph: &subgraph,
            subgraph_name: &fetch.subgraph_name,
            sharing: &self.sharing,
            supergraph: &self.schema_view,
            algebra,
            producer,
        };
        let selections = remove_context_arguments(
            &context_variables,
            entity_selections.unwrap_or(operation.selections),
        );
        response.produce_fetch(
            &production,
            Mount {
                copy: position.copy,
                base,
                cases,
            },
            &selections,
            &fetch.output_rewrites,
        )
    }

    /// The configurations a fetch runs at its position in: the model's `FetchSite.ActiveAt` with
    /// `fetchPositions`. Its path's type conditions, the condition-node branches it runs under and,
    /// for an entity fetch, the runtime types some `requires` entry admits, read in the
    /// supergraph, an interface entry standing for its objects and an entry with no type
    /// condition, or a field, admitting every object: the model's `FetchNode.admitsPosition`. An
    /// entry's Boolean conditions do not restrict where the fetch runs; where they exclude every
    /// entry the representation is empty, and what it owes there is the representation check's.
    /// `None` where it runs nowhere.
    fn site_region(
        &self,
        response: &Response,
        position: &Position,
        condition: &[BooleanLiteral],
        fetch: &FetchNode,
    ) -> Option<Clause> {
        let algebra = &self.algebra;
        let mut region = algebra.conjoin(
            &position.region,
            &algebra.clause(condition.iter().map(boolean))?,
        )?;
        let mut admitted: Vec<Name> = Vec::new();
        for entry in &fetch.requires {
            let requires_selection::Selection::InlineFragment(fragment) = entry else {
                return Some(region);
            };
            let Some(type_name) = &fragment.type_condition else {
                return Some(region);
            };
            admitted.push(type_name.clone());
            admitted.extend(self.schema_view.possible_types(type_name).iter().cloned());
        }
        if !fetch.requires.is_empty() {
            let literal =
                response.type_literal(algebra, position.copy, algebra.objects(&admitted))?;
            region = algebra.conjoin(&region, &literal)?;
        }
        Some(region)
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
const ENTITIES_FIELD_NAME: Name = name!("_entities");

/// What an entity fetch asks of the entities it was given, with its shape checked.
///
/// This is the model's `entityOperationWellFormed`: exactly one root field, `_entities`. What is
/// selected under it must all be type-conditioned inline fragments, which
/// [`representation::entity_fragments`] checks where it reads them off.
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
    /// Is an entity fetch's representation what its subgraph is owed, wherever it runs: the
    /// model's `checkEntityFetch`, its contextual availability asked beside it in
    /// [`Self::walk_fetch`]. The `_entities` body is read first, every selection there an inline
    /// fragment carrying a type condition; then everything the representation reads was fetched
    /// under the fields the entries name, its explicit selections agree with the input rewrites,
    /// and it covers what the subgraph demands, through a resolvable key in each configuration
    /// (`representation.rs`).
    #[allow(clippy::too_many_arguments)]
    fn check_entity_fetch(
        &self,
        fetch: &FetchNode,
        subgraph: &Subgraph<'_>,
        entity_body: &[Selection],
        entity_types: &[(Name, Vec<Name>)],
        projection: &Projection,
        held: View<'_>,
        site: &Site<'_>,
    ) -> Result<(), ComparisonError> {
        let fragments = entity_fragments(fetch, entity_body)?;
        check_inputs_available(site, projection)?;
        let agreement = Agreement::new(
            self.supergraph_schema,
            &fetch.input_rewrites,
            site.position.copy,
        );
        check_explicit_agreement(site, projection, &agreement)?;
        self.check_representations(
            fetch,
            subgraph,
            &fragments,
            entity_types,
            projection,
            held,
            &agreement,
            site,
        )
    }
}

//==================================================================================================
// The Boolean boundary
//==================================================================================================

/// Is every Boolean variable the plan's conditions read declared by the operation as `Boolean` or
/// `Boolean!`? The model's `queryPlanBooleanError`, checked before anything is read: a condition
/// node's variable, each `@skip` and `@include` of a fetch's operation, and each condition of a
/// `requires` entry at any depth, read before a literal is folded, so a variable beside
/// `@include(if: false)`, or under both `@include` and `@skip`, is read all the same. The checker's
/// configurations give each such variable a Boolean value; a variable the operation does not
/// declare, or declares as anything but a `Boolean`, has none, and the router's
/// `Condition::is_included` excludes nothing for a variable it cannot resolve, so reading one as
/// excluding, or as contradicting another condition, would be unsound. The plan is refused, the
/// variable named.
///
/// The client operation's own conditions are not read: its validity, which the checker assumes,
/// has each variable they read declared as a `Boolean` or a `Boolean!`. Each reference is looked
/// up by name among the declared Booleans, so the boundary reads each declaration once and each
/// reference once.
///
/// A declaration is what is checked, not a value. The configurations are complete Boolean
/// assignments, a value overriding a default included; whether every variable read is given a
/// Boolean by the request or by a default, which a nullable `Boolean` omitted with no default, or
/// passed `null`, is not, is the model's separate `queryPlanBooleanValuesError` over supplied
/// values. `check_plan` is given no values and does not decide it, and the router does not refuse
/// such a request either: it reads the condition as excluding nothing.
fn check_boolean_variables(
    algebra: &Algebra,
    variables: &[Node<ast::VariableDefinition>],
    plan: &QueryPlan,
) -> Result<(), ComparisonError> {
    let mut read: Vec<Name> = Vec::new();
    match &plan.node {
        None => {}
        Some(TopLevelPlanNode::Subscription(subscription)) => {
            fetch_variables(algebra, &subscription.primary, &mut read);
            if let Some(rest) = &subscription.rest {
                plan_variables(algebra, rest, &mut read);
            }
        }
        Some(TopLevelPlanNode::Fetch(fetch)) => fetch_variables(algebra, fetch, &mut read),
        Some(TopLevelPlanNode::Sequence(node)) => {
            for node in &node.nodes {
                plan_variables(algebra, node, &mut read);
            }
        }
        Some(TopLevelPlanNode::Parallel(node)) => {
            for node in &node.nodes {
                plan_variables(algebra, node, &mut read);
            }
        }
        Some(TopLevelPlanNode::Flatten(node)) => plan_variables(algebra, &node.node, &mut read),
        Some(TopLevelPlanNode::Defer(node)) => defer_variables(algebra, node, &mut read),
        Some(TopLevelPlanNode::Condition(node)) => condition_variables(algebra, node, &mut read),
    }
    let declared: HashSet<&Name> = variables
        .iter()
        .filter(|definition| {
            matches!(
                definition.ty.as_ref(),
                ast::Type::Named(name) | ast::Type::NonNullNamed(name) if name == "Boolean"
            )
        })
        .map(|definition| &definition.name)
        .collect();
    for variable in read {
        if !declared.contains(&variable) {
            return Err(ComparisonError::new(format!(
                "the plan conditions on `${variable}`, which the operation does not declare as \
                 `Boolean` or `Boolean!`"
            )));
        }
    }
    Ok(())
}

/// The Boolean variables a plan node's conditions read: [`check_boolean_variables`]'s walk.
fn plan_variables(algebra: &Algebra, node: &PlanNode, read: &mut Vec<Name>) {
    match node {
        PlanNode::Fetch(fetch) => fetch_variables(algebra, fetch, read),
        PlanNode::Sequence(node) => {
            for node in &node.nodes {
                plan_variables(algebra, node, read);
            }
        }
        PlanNode::Parallel(node) => {
            for node in &node.nodes {
                plan_variables(algebra, node, read);
            }
        }
        PlanNode::Flatten(node) => plan_variables(algebra, &node.node, read),
        PlanNode::Defer(node) => defer_variables(algebra, node, read),
        PlanNode::Condition(node) => condition_variables(algebra, node, read),
    }
}

fn defer_variables(algebra: &Algebra, node: &crate::query_plan::DeferNode, read: &mut Vec<Name>) {
    if let Some(primary) = &node.primary.node {
        plan_variables(algebra, primary, read);
    }
    for block in &node.deferred {
        if let Some(deferred) = &block.node {
            plan_variables(algebra, deferred, read);
        }
    }
}

fn condition_variables(
    algebra: &Algebra,
    node: &crate::query_plan::ConditionNode,
    read: &mut Vec<Name>,
) {
    algebra.work.boolean_references.add(1);
    read.push(node.condition_variable.clone());
    for clause in [&node.if_clause, &node.else_clause].into_iter().flatten() {
        plan_variables(algebra, clause, read);
    }
}

/// A fetch's: its `requires` entries' conditions, and the `@skip` and `@include` of its operation,
/// its fragments' included. An operation that does not parse reads nothing here; the walk reports
/// it.
fn fetch_variables(algebra: &Algebra, fetch: &FetchNode, read: &mut Vec<Name>) {
    let mut named = Vec::new();
    requires_variables(algebra, &fetch.requires, &mut named);
    read.extend(named.into_iter().cloned());
    let Ok(document) = fetch.operation_document.as_parsed() else {
        return;
    };
    for operation in document.operations.iter() {
        selection_variables(algebra, &operation.selection_set.selections, read);
    }
    for fragment in document.fragments.values() {
        selection_variables(algebra, &fragment.selection_set.selections, read);
    }
}

fn selection_variables(algebra: &Algebra, selections: &[Selection], read: &mut Vec<Name>) {
    for selection in selections {
        let (directives, below) = match selection {
            Selection::Field(field) => (&field.directives, Some(&field.selection_set.selections)),
            Selection::InlineFragment(fragment) => (
                &fragment.directives,
                Some(&fragment.selection_set.selections),
            ),
            Selection::FragmentSpread(spread) => (&spread.directives, None),
        };
        for directive in directives.iter() {
            if directive.name != "skip" && directive.name != "include" {
                continue;
            }
            algebra.work.boolean_references.add(1);
            if let Some(ast::Value::Variable(variable)) = directive
                .specified_argument_by_name("if")
                .map(|value| value.as_ref())
            {
                read.push(variable.clone());
            }
        }
        if let Some(below) = below {
            selection_variables(algebra, below, read);
        }
    }
}

//==================================================================================================
// The client operation
//==================================================================================================

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
    check_plan_counted(
        supergraph_schema,
        subgraphs_by_name,
        operation_doc,
        plan,
        options,
    )
    .0
}

/// [`check_plan_with_options`], with the work it took: the counters the growth laws read.
fn check_plan_counted(
    supergraph_schema: &ValidFederationSchema,
    subgraphs_by_name: &IndexMap<Arc<str>, ValidFederationSchema>,
    operation_doc: &Valid<ExecutableDocument>,
    plan: &QueryPlan,
    options: CheckerOptions,
) -> (Result<(), ComparisonError>, Work) {
    let operation = match operation_doc.operations.get(None) {
        Ok(operation) => operation,
        Err(_) => {
            return (
                Err(ComparisonError::new(
                    "expected exactly one operation in the input document".to_string(),
                )),
                Work::default(),
            );
        }
    };
    let Some(root_type) = supergraph_schema
        .schema()
        .root_operation(operation.operation_type)
        .cloned()
    else {
        return (
            Err(ComparisonError::new(format!(
                "schema has no {} root type",
                operation.operation_type
            ))),
            Work::default(),
        );
    };
    let checker = match Checker::new(supergraph_schema, subgraphs_by_name, options) {
        Ok(checker) => checker,
        Err(e) => return (Err(e), Work::default()),
    };
    let verdict = checker.check(
        operation_doc,
        &operation.variables,
        &operation.selection_set.selections,
        root_type,
        plan,
    );
    (verdict, checker.algebra.work)
}

impl Checker<'_> {
    /// Checks the Boolean variables the plan reads, then walks the plan, checking each fetch where
    /// it is reached, then the client operation against everything fetched.
    fn check(
        &self,
        operation_doc: &Valid<ExecutableDocument>,
        variables: &[Node<ast::VariableDefinition>],
        selections: &[Selection],
        root_type: Name,
        plan: &QueryPlan,
    ) -> Result<(), ComparisonError> {
        check_boolean_variables(&self.algebra, variables, plan)?;
        let mut response = Response::new(root_type);
        let fetched = match &plan.node {
            Some(node) => self.walk_top_level(&mut response, node)?,
            // A plan with no node fetches nothing, which is correct only for an operation that
            // asks for nothing fetchable.
            None => Vec::new(),
        };

        // Completeness: the response the plan fetches covers the client operation, at every
        // admissible configuration of the copies it fetched.
        let client = without_unfetched_introspection(selections, &operation_doc.fragments, true);
        let view = response.view(&fetched);
        let hypothesis = Hypothesis::new(&self.algebra, view);
        Coverage {
            schema: &self.schema_view,
            fragments: &operation_doc.fragments,
            hypothesis: &hypothesis,
            supplied: &Supplies::new(view),
            supplier: Supplier::Plan,
            agreement: None,
        }
        .covers(
            CopyId::ROOT,
            Demand::of(&self.algebra, everything(), client.iter().collect()),
        )
        .map_err(|uncovered| {
            ComparisonError::new(format!(
                "query plan does not fetch everything the operation requests:\n{uncovered}"
            ))
        })
    }
}
