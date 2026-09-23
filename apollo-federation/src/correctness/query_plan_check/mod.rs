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
//! - **Soundness** — every entity fetch's `requires` is matched by its entity cases, checked where
//!   the fetch is reached, while the state that reached it is in hand. No fetch site is ever
//!   collected.
//!
//! # Differences from the model
//!
//! - **The subgraph oracle.** The model has no notion of which subgraphs can resolve a field; the
//!   legacy checker does, through `SubgraphConstraint`. That oracle is passed to
//!   `includes_with_constraint` here, so this reaches parity with legacy rather than with the
//!   model alone.
//! - **No fuel.** `conditionMatchesRequirementFuel` carries a budget that is an artifact of Lean's
//!   termination checker; the model's own notes say an implementation should simply loop.
//! - **`@context` data availability.** Every contextual value a fetch reads must already be
//!   fetched wherever the fetch runs. The legacy checker does not check this at all: at run time
//!   a rewrite path that reaches nothing simply drops the argument, so nothing notices.
//! - **Validity** and **condition variables** — the two sections below.
//!
//! # Validity of the operations the walk builds
//!
//! The model assumes the operations it compares are valid and carries that as a premise of its
//! correctness statement, `QueryPlanWellFormed`, naming three: the plan read as an operation, what
//! each entity fetch has already fetched, and what each `@key` demands of it. The checker does not
//! decide it for any of them.
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

use self::context::check_context_rewrites;
use self::context::context_variables;
use self::context::remove_context_arguments;
use self::requires::condition_matches_requirement;
use self::requires::key_half;
use self::requires::requires_half;
use self::requires::to_requires_field_set;
use self::selections::apply_output_rewrites;
use self::selections::inline_fragment_spreads;
use self::selections::selections_at;
use self::selections::under_condition;
use self::subgraph::KeyDirective;
use self::subgraph::Subgraph;
use super::query_compare;
use super::query_compare::conditions::BooleanLiteral;
use super::response_shape_compare::ComparisonError;
use super::subgraph_constraint::SubgraphConstraint;
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
    ) -> Result<Self, ComparisonError> {
        Ok(Checker {
            supergraph_schema,
            comparator: query_compare::QueryComparator::new(supergraph_schema)
                .map_err(|e| ComparisonError::new(e.to_string()))?,
            subgraphs_by_name,
            operation,
            constraint: SubgraphConstraint::new(subgraphs_by_name),
            condition_variables: condition_variables(plan),
            root_type,
        })
    }
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
    fn walk_node(
        &self,
        node: &PlanNode,
        reached: Reached<'_>,
        available: &[Selection],
    ) -> Result<Vec<Selection>, ComparisonError> {
        match node {
            PlanNode::Fetch(fetch) => self.walk_fetch(fetch, reached, available),
            PlanNode::Sequence(sequence) => {
                // Each child sees what the previous ones added.
                let mut added: Vec<Selection> = Vec::new();
                for child in &sequence.nodes {
                    let mut seen = available.to_vec();
                    seen.extend(added.iter().cloned());
                    added.extend(self.walk_node(child, reached, &seen)?);
                }
                Ok(added)
            }
            PlanNode::Parallel(parallel) => {
                // Every child sees the same input.
                let mut added: Vec<Selection> = Vec::new();
                for child in &parallel.nodes {
                    added.extend(self.walk_node(child, reached, available)?);
                }
                Ok(added)
            }
            PlanNode::Flatten(flatten) => {
                let mut path = reached.path.to_vec();
                path.extend(flatten.path.iter().cloned());
                self.walk_node(
                    &flatten.node,
                    Reached {
                        path: &path,
                        condition: reached.condition,
                    },
                    available,
                )
            }
            PlanNode::Defer(defer) => {
                let primary = match &defer.primary.node {
                    Some(node) => self.walk_node(node, reached, available)?,
                    None => Vec::new(),
                };
                // Deferred blocks start after the primary block.
                let mut seen = available.to_vec();
                seen.extend(primary.iter().cloned());
                let mut added = primary;
                for block in &defer.deferred {
                    if let Some(node) = &block.node {
                        added.extend(self.walk_node(node, reached, &seen)?);
                    }
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
                            path: reached.path,
                            condition: &condition,
                        },
                        available,
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
        };
        let fetched = match node {
            TopLevelPlanNode::Subscription(subscription) => {
                // The primary fetch opens the stream, so it precedes the rest of the plan.
                let primary = self.walk_fetch(&subscription.primary, reached, &[])?;
                let mut added = primary.clone();
                if let Some(rest) = &subscription.rest {
                    added.extend(self.walk_node(rest, reached, &primary)?);
                }
                added
            }
            TopLevelPlanNode::Fetch(fetch) => {
                self.walk_node(&PlanNode::Fetch(fetch.clone()), reached, &[])?
            }
            TopLevelPlanNode::Sequence(node) => {
                self.walk_node(&PlanNode::Sequence(node.clone()), reached, &[])?
            }
            TopLevelPlanNode::Parallel(node) => {
                self.walk_node(&PlanNode::Parallel(node.clone()), reached, &[])?
            }
            TopLevelPlanNode::Flatten(node) => {
                self.walk_node(&PlanNode::Flatten(node.clone()), reached, &[])?
            }
            TopLevelPlanNode::Defer(node) => {
                self.walk_node(&PlanNode::Defer(node.clone()), reached, &[])?
            }
            TopLevelPlanNode::Condition(node) => {
                self.walk_node(&PlanNode::Condition(node.clone()), reached, &[])?
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
    ) -> Result<Vec<Selection>, ComparisonError> {
        let body = self.fetch_body(fetch)?;
        // A root fetch selects its whole operation and requires nothing; an entity fetch selects
        // what its operation asks of the entities it was given.
        let selected = if fetch.requires.is_empty() {
            body
        } else {
            let entity_body = entity_body(fetch, &body)?;
            self.check_fetch(fetch, &entity_body, reached, available)?;
            entity_body
        };
        // The entity cases were read off the fetch's own operation above; what it *contributes*
        // is that selection set under the key renames its output rewrites apply and without the
        // synthetic arguments its context rewrites bind. `interpret_fetch_node` applies the two
        // in this order.
        let contributed =
            apply_output_rewrites(self.supergraph_schema, &fetch.output_rewrites, selected);
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

    /// A fetch's own operation, its fragment spreads inlined.
    fn fetch_body(&self, fetch: &FetchNode) -> Result<Vec<Selection>, ComparisonError> {
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
        inline_fragment_spreads(&operation.selection_set.selections, &document.fragments)
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
        entity_body: &[Selection],
        reached: Reached<'_>,
        available: &[Selection],
    ) -> Result<(), ComparisonError> {
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

        let subgraph_schema = self
            .subgraphs_by_name
            .get(fetch.subgraph_name.as_ref())
            .ok_or_else(|| {
                ComparisonError::new(format!(
                    "fetch to unknown subgraph `{}`",
                    fetch.subgraph_name
                ))
            })?;
        let subgraph = Subgraph::new(subgraph_schema)?;

        // Bound once per fetch, not per entry or per `@key`.
        let available_doc = self.operation_with(available.to_vec());

        // Only `key.fields` varies across the keys tried, so hoist the rest per case.
        let mut per_case = Vec::with_capacity(cases.len());
        for (entity_type, entity_selections) in &cases {
            let selections = requires_half(
                self.supergraph_schema,
                &subgraph,
                entity_type,
                entity_selections,
            )?;
            let field_set = to_requires_field_set(&selections);
            per_case.push((subgraph.keys(entity_type)?, selections, field_set));
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
                    &per_case[case],
                    reached,
                    available,
                    &available_doc,
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
            fetch,
        )
    }

    /// Whether one `requires` entry is matched by one entity case: the subgraph resolves that
    /// case from a key the plan has already fetched, and the entry declares exactly that key.
    ///
    /// `None` means matched; `Some(why)` says why not, per `@key` tried. An `Err` is a malformed
    /// plan or schema, not a mismatch.
    ///
    /// The `@key`-does-not-apply arm below has no test, and neither `tests.rs` nor the fuzz plan
    /// lane reaches it. It needs two things at once: entity types whose keys name different
    /// fields, so that one type's key can fail to typecheck at another's entry, and a plan whose
    /// pairs of equal type do not settle the table, so that the search looks at any other pair at
    /// all. A plan the planner produced never supplies the second. Covering it means building a
    /// plan whose diagonal fails, the way the fuzz lane's perturbations build wrong plans.
    #[allow(clippy::too_many_arguments)]
    fn requirement_matches_case(
        &self,
        require_item: &requires_selection::Selection,
        require_type: &Name,
        entity_type: &Name,
        hoisted: &(
            Vec<KeyDirective>,
            Vec<Selection>,
            Vec<requires_selection::Selection>,
        ),
        reached: Reached<'_>,
        available: &[Selection],
        available_doc: &Valid<ExecutableDocument>,
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
            if !condition_matches_requirement(
                self.supergraph_schema,
                std::slice::from_ref(&require_item),
                &demanded.iter().collect::<Vec<_>>(),
            ) {
                unmatched.push(format!(
                    "  @key({}): the entry does not declare what the subgraph demands",
                    key.fields
                ));
                continue;
            }
            // Mounted where the fetch runs and guarded by the condition it runs under, so the
            // comparison happens at the query root.
            let mut computed = key_selections;
            computed.extend(requires_selections.iter().cloned());
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
            if let Err(e) = self.comparator.includes_with_constraint(
                &self.constraint,
                available_doc,
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

/// The entity cases of a fetch: the type condition and selections of each `_entities` inline
/// fragment, one per entity type the fetch asks about.
///
/// Entries and cases are matched many-to-many, and the plan does not record the matching.
fn entity_cases<'a>(
    fetch: &FetchNode,
    entity_body: &'a [Selection],
) -> Result<Vec<(&'a Name, &'a [Selection])>, ComparisonError> {
    entity_body
        .iter()
        .map(|selection| match selection {
            Selection::InlineFragment(fragment) => match &fragment.type_condition {
                Some(entity_type) => {
                    Ok((entity_type, fragment.selection_set.selections.as_slice()))
                }
                None => Err(ComparisonError::new(format!(
                    "fetch to {}: an `{ENTITIES_FIELD_NAME}` selection must carry a type condition",
                    fetch.subgraph_name
                ))),
            },
            _ => Err(ComparisonError::new(format!(
                "fetch to {}: `{ENTITIES_FIELD_NAME}` must select only inline fragments",
                fetch.subgraph_name
            ))),
        })
        .collect()
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
    )?;
    let Some(node) = &plan.node else {
        // A plan with no node fetches nothing, which is correct only for an operation that asks
        // for nothing fetchable.
        return Ok(());
    };
    let walked = checker.walk_top_level(node)?;

    // Completeness: everything the plan fetches, read as one operation, must include the client
    // operation. The subgraph oracle is what takes this past the model, which knows nothing about
    // which subgraphs can resolve a field.
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
