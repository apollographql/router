//! Whether what was fetched covers a demand: the model's `ResponseCovers`, decided per copy over
//! symbolic configurations rather than per configuration.
//!
//! The model asks coverage under every Boolean assignment and every configuration of the copies.
//! Here a demand reaching a copy is a list of entries, each a region and the selections demanded
//! under it, and every obligation is decided under the hypothesis (`hypothesis.rs`): for each
//! response key demanded at the copy's possible runtime types,
//! - some contribution under the key is active wherever a demanded field is, so a key fetched only
//!   under a guard, a runtime type or a parent the demand does not imply is missing;
//! - every contribution that can be active with a demanded field makes the demanded call, with its
//!   result declared, so a missing declaration or an unjustified correspondence is owed rather than
//!   excused;
//! - the copy the key holds is owed the demanded subselections, wherever it is present.
//!
//! A copy is visited once per walk, its demand merged from its one parent: entries with the same
//! selections, below fields sent under the same names, are one entry under the union of their
//! regions.
//!
//! The same walk decides what an entity fetch's representation owes its subgraph, over the view
//! of what the representation holds (`representation.rs`), with one more obligation per demanded
//! field: the fetch's input rewrites carry the key it is held under to the field it names, and
//! back, read under the names its own demand's fields above it are sent as, so that one key held
//! for different fields at different runtime types is read under each one's names. Where no
//! single key of the entity covers every configuration, the walk collects the configurations each
//! key fails in instead of stopping at the first, so that the key can be chosen per configuration.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;
use std::rc::Rc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::FragmentMap;
use apollo_compiler::executable::Selection;

use super::super::query_compare::conditions::of_type_region_under;
use super::super::query_compare::same_arguments;
use super::super::query_compare::schema_view::SchemaView;
use super::hypothesis::Hypothesis;
use super::region::Algebra;
use super::region::Clause;
use super::region::Literal;
use super::region::Region;
use super::region::TypeSet;
use super::region::Value;
use super::region::Var;
use super::region::boolean;
use super::representation::Agreement;
use super::response::Contribution;
use super::response::CopyId;
use super::response::Response;
use super::response::View;

/// What is demanded on one copy: selections, each under the region it is demanded in and below the
/// fields it is sent under, one entry per distinct list of selections and names, found by the
/// list's identity rather than by comparing it with every entry, and each entry's region kept as
/// the union of the regions it was demanded in.
#[derive(Default)]
pub(super) struct Demand<'d> {
    entries: Vec<Entry<'d>>,
    /// Each entry by its identity, and the clauses of its region.
    index: HashMap<EntryKey, (usize, HashSet<Clause>)>,
}

/// An entry's identity: the names it is sent under and the addresses of its selections.
type EntryKey = (Vec<Name>, Vec<usize>);

/// Selections demanded in a region.
struct Entry<'d> {
    region: Region,
    /// The names the fields above the selections are sent as, from the agreement's anchor down:
    /// what the rewrite agreement reads a demanded field under. Empty where no agreement is read.
    sent: Vec<Name>,
    selections: Vec<&'d Selection>,
}

impl<'d> Demand<'d> {
    /// One list of selections demanded in one region, at the copy a walk starts at.
    pub(super) fn of(algebra: &Algebra, region: Region, selections: Vec<&'d Selection>) -> Self {
        let mut demand = Demand::default();
        demand.add(algebra, region, Vec::new(), selections);
        demand
    }

    /// Adds selections demanded in a region below the fields sent as `sent`, merging them into the
    /// entry with the same list and names.
    fn add(
        &mut self,
        algebra: &Algebra,
        region: Region,
        sent: Vec<Name>,
        selections: Vec<&'d Selection>,
    ) {
        algebra.work.demand_entries.add(1);
        algebra
            .work
            .demand_key_width
            .add((sent.len() + selections.len()) as u64);
        let key: EntryKey = (
            sent,
            selections
                .iter()
                .map(|selection| std::ptr::from_ref(*selection) as usize)
                .collect(),
        );
        match self.index.get_mut(&key) {
            Some((at, known)) => {
                algebra.work.demand_merges.add(1);
                for clause in region {
                    algebra.work.demand_key_width.add(clause.len() as u64);
                    if known.insert(clause.clone()) {
                        self.entries[*at].region.push(clause);
                    }
                }
            }
            None => {
                let known: HashSet<Clause> = region.iter().cloned().collect();
                algebra
                    .work
                    .demand_key_width
                    .add(region.iter().map(|clause| clause.len() as u64).sum());
                let sent = key.0.clone();
                self.index.insert(key, (self.entries.len(), known));
                self.entries.push(Entry {
                    region,
                    sent,
                    selections,
                });
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Why a demand is not covered, and where: the copies descended through, outermost first, and
/// what failed at the last.
#[derive(Debug)]
pub(super) struct Uncovered {
    trail: Vec<String>,
    reason: String,
}

impl Uncovered {
    pub(super) fn at(reason: String) -> Self {
        Uncovered {
            trail: Vec::new(),
            reason,
        }
    }

    fn under(mut self, frame: String) -> Self {
        self.trail.insert(0, frame);
        self
    }
}

impl fmt::Display for Uncovered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for frame in &self.trail {
            writeln!(f, "  under {frame}")?;
        }
        write!(f, "  --> {}", self.reason)
    }
}

/// A clause as an explanation writes it, ` when …`, or nothing for every configuration.
pub(super) fn render_clause(algebra: &Algebra, response: &Response, clause: &Clause) -> String {
    if clause.is_empty() {
        return String::new();
    }
    let literals: Vec<String> = clause
        .iter()
        .map(|literal| match (&literal.var, literal.value) {
            (Var::Boolean(variable), Value::Is(true)) => format!("${variable}"),
            (Var::Boolean(variable), _) => format!("¬${variable}"),
            (Var::Copy(copy), Value::Types(types)) => {
                format!("{} is {}", response.describe(*copy), algebra.render(types))
            }
            (var, _) => format!("{var:?}"),
        })
        .collect();
    format!(" when {}", literals.join(" ∧ "))
}

/// A copy and a configuration as an explanation writes them: the copy, with the runtime types the
/// configuration gives it, and the rest of the configuration.
pub(super) fn describe_at(
    hypothesis: &Hypothesis<'_>,
    copy: CopyId,
    mut example: Clause,
) -> String {
    let algebra = hypothesis.algebra;
    let response = hypothesis.view.response;
    if copy == CopyId::ROOT {
        return format!(
            "{}{}",
            response.describe(copy),
            render_clause(algebra, response, &example)
        );
    }
    let own = Var::Copy(copy);
    let types = match example.iter().position(|literal| literal.var == own) {
        Some(at) => match example.remove(at).value {
            Value::Types(types) => types,
            Value::Is(_) => hypothesis.object_domain(copy),
        },
        None => hypothesis.object_domain(copy),
    };
    format!(
        "{}, a {}{}",
        response.describe(copy),
        algebra.render(algebra.minus(types, algebra.absent())),
        render_clause(algebra, response, &example)
    )
}

/// A field demanded at a copy, in the region it is demanded in, below the fields sent as `sent`.
struct Asked<'d, 'e> {
    field: &'d Field,
    region: Region,
    sent: &'e [Name],
}

/// The names a demanded field is sent under from the agreement's anchor: its demand's, then its
/// own.
fn sent_as(demanded: &Asked<'_, '_>) -> Vec<Name> {
    let mut names = demanded.sent.to_vec();
    names.push(demanded.field.name.clone());
    names
}

/// What a view supplies at each copy and key: the guards of its contributions there, as
/// [`Hypothesis::supplies`] gives them, computed once per copy and key for every walk over the view.
pub(super) struct Supplies<'s> {
    pub(super) view: View<'s>,
    simplified: RefCell<HashMap<(CopyId, Name), Covers>>,
}

/// The clauses supplying one copy's key, shared by the walks reading them.
type Covers = Rc<Vec<Clause>>;

impl<'s> Supplies<'s> {
    pub(super) fn new(view: View<'s>) -> Self {
        Supplies {
            view,
            simplified: Default::default(),
        }
    }

    pub(super) fn at(&self, hypothesis: &Hypothesis<'_>, copy: CopyId, key: &Name) -> Covers {
        let entry = (copy, key.clone());
        if let Some(supplies) = self.simplified.borrow().get(&entry) {
            return supplies.clone();
        }
        let supplies = Rc::new(
            hypothesis.supplies(
                self.view
                    .fields_at(copy, key)
                    .flat_map(|field| field.guard.iter().cloned())
                    .collect(),
            ),
        );
        self.simplified.borrow_mut().insert(entry, supplies.clone());
        supplies
    }
}

/// What supplies a demand, as an explanation names it.
#[derive(Clone, Copy)]
pub(super) enum Supplier {
    /// What the plan fetched, which the client operation is owed.
    Plan,
    /// What an entity fetch sends its subgraph: its representation.
    Representation,
}

impl Supplier {
    fn missing(self, key: &Name, at: &str) -> String {
        match self {
            Supplier::Plan => format!("the plan does not select `{key}` on {at}"),
            Supplier::Representation => {
                format!("the representation does not hold `{key}` at {at}")
            }
        }
    }

    fn holds(self, key: &Name, at: &str) -> String {
        match self {
            Supplier::Plan => format!("the plan selects `{key}` on {at}"),
            Supplier::Representation => format!("the representation holds `{key}` at {at}"),
        }
    }
}

/// What a walk does with an obligation that fails.
enum Sink {
    /// Stops there, with why: whether the demand is covered.
    First,
    /// Records every configuration it fails in and goes on: where the demand fails.
    All(Region),
}

/// A coverage walk: the client operation over everything fetched, or what a fetch's subgraph
/// demands over the representation it sends.
pub(super) struct Coverage<'c, 'd> {
    pub(super) schema: &'c SchemaView<'c>,
    pub(super) fragments: &'d FragmentMap,
    /// The admissible configurations, from what was fetched before.
    pub(super) hypothesis: &'c Hypothesis<'c>,
    /// What supplies the demand.
    pub(super) supplied: &'c Supplies<'c>,
    pub(super) supplier: Supplier,
    /// For a representation, the input rewrites every demanded field it sends agrees with: the
    /// demand's own half of the model's `requireAgreesWithRewrites`, the walk starting at the copy
    /// the agreement is anchored at and each field read under its own demand's names.
    pub(super) agreement: Option<&'c Agreement<'c>>,
}

impl<'d> Coverage<'_, 'd> {
    /// Is every field demanded on a copy fetched there, and on the copies below it? The model's
    /// `ResponseCovers`, at every admissible configuration and every assignment.
    pub(super) fn covers(&self, copy: CopyId, demand: Demand<'d>) -> Result<(), Uncovered> {
        self.walk(copy, demand, &mut Sink::First)
    }

    /// Where the demand on a copy fails: the configurations every failed obligation fails in, so
    /// that they can be read beside another demand's. `Err` only for a demand that cannot be read.
    pub(super) fn failures(&self, copy: CopyId, demand: Demand<'d>) -> Result<Region, Uncovered> {
        let mut sink = Sink::All(Vec::new());
        self.walk(copy, demand, &mut sink)?;
        match sink {
            Sink::All(failing) => Ok(failing),
            Sink::First => Ok(Vec::new()),
        }
    }

    /// One obligation decided under the hypothesis: every configuration of `demand` in which the
    /// copy is present lies in some clause of `supplies`.
    fn obligation(
        &self,
        copy: CopyId,
        demand: &Clause,
        supplies: &[Clause],
        sink: &mut Sink,
        why: impl Fn(Clause) -> Uncovered,
    ) -> Result<(), Uncovered> {
        match sink {
            Sink::First => match self.hypothesis.uncovered(copy, demand, supplies) {
                Some(example) => Err(why(example)),
                None => Ok(()),
            },
            Sink::All(failing) => {
                failing.extend(self.hypothesis.uncovered_region(copy, demand, supplies));
                Ok(())
            }
        }
    }

    fn walk(&self, copy: CopyId, demand: Demand<'d>, sink: &mut Sink) -> Result<(), Uncovered> {
        let hypothesis = self.hypothesis;
        let algebra = hypothesis.algebra;
        let response = self.supplied.view.response;
        algebra.work.copies_visited.add(1);
        let completion = hypothesis.completion();
        let mut groups: IndexMap<&'d Name, Vec<Asked<'d, '_>>> = IndexMap::default();
        for entry in &demand.entries {
            // The runtime types the entry is demanded at, read together.
            let types = algebra.names(hypothesis.allowed(copy, &entry.region));
            if types.is_empty() {
                continue;
            }
            let fields = of_type_region_under(
                self.schema,
                self.fragments,
                &types,
                &[(Vec::new(), entry.selections.clone())],
            )
            .map_err(|e| Uncovered::at(e.to_string()))?;
            for asked in fields {
                let narrowed = if asked.condition.possible_types.len() == types.len() {
                    Some(Vec::new())
                } else {
                    response.type_literal(
                        algebra,
                        copy,
                        algebra.objects(&asked.condition.possible_types),
                    )
                };
                let Some(narrowed) = narrowed.and_then(|narrowed| {
                    algebra.conjoin(
                        &narrowed,
                        &algebra.clause(asked.condition.boolean_condition.iter().map(boolean))?,
                    )
                }) else {
                    continue;
                };
                let region = algebra.simplify(
                    algebra.product(&entry.region, std::slice::from_ref(&narrowed)),
                    &completion,
                );
                if region.is_empty() {
                    continue;
                }
                groups
                    .entry(asked.response_name())
                    .or_default()
                    .push(Asked {
                        field: asked.field,
                        region,
                        sent: &entry.sent,
                    });
            }
        }

        for (key, asked) in &groups {
            let supplied: Vec<&Contribution> = self.supplied.view.fields_at(copy, key).collect();
            let supplies = self.supplied.at(hypothesis, copy, key);
            for demanded in asked {
                for clause in &demanded.region {
                    self.obligation(copy, clause, &supplies, sink, |example| {
                        Uncovered::at(
                            self.supplier
                                .missing(key, &describe_at(self.hypothesis, copy, example)),
                        )
                    })?;
                }
            }
            if self.one_call(copy, asked) {
                for demanded in asked {
                    for field in &supplied {
                        let wrong_call = !makes_call(field, demanded.field);
                        if !wrong_call && field.result.is_some() {
                            continue;
                        }
                        let (joint, example) = self.joint(
                            copy,
                            &demanded.region,
                            &field.guard,
                            matches!(sink, Sink::All(_)),
                        );
                        let Some(example) = example else {
                            continue;
                        };
                        if let Sink::All(failing) = sink {
                            failing.extend(joint);
                            continue;
                        }
                        let at = describe_at(self.hypothesis, copy, example);
                        let holds = self.supplier.holds(key, &at);
                        return Err(Uncovered::at(if wrong_call {
                            format!(
                                "{holds} as `{}`, where `{}` is asked",
                                render_call(&field.field, &field.arguments),
                                render_call(&demanded.field.name, &demanded.field.arguments),
                            )
                        } else {
                            format!(
                                "{holds}, and `{}.{}` in `{}` has a missing declaration or an \
                                 unjustified correspondence",
                                field.subgraph_parent,
                                field.field,
                                response.producer(field.producer),
                            )
                        }));
                    }
                }
            } else {
                let failing =
                    self.no_call_matches(copy, asked, &supplied, matches!(sink, Sink::All(_)));
                if let Some(first) = failing.first() {
                    match sink {
                        Sink::First => {
                            return Err(Uncovered::at(format!(
                                "{} as a call no demanded field makes{}",
                                self.supplier.holds(key, &response.describe(copy)),
                                render_clause(algebra, response, first),
                            )));
                        }
                        Sink::All(failed) => failed.extend(failing),
                    }
                }
            }
            if let Some(agreement) = self.agreement {
                for demanded in asked {
                    let names = sent_as(demanded);
                    let agrees = agreement.region(algebra, response, copy, key, &names);
                    for clause in &demanded.region {
                        self.obligation(copy, clause, &agrees, sink, |example| {
                            Uncovered::at(format!(
                                "the representation sends `{key}` at {} as `{}`, and the fetch's \
                                 input rewrites do not carry the one to the other",
                                describe_at(self.hypothesis, copy, example),
                                demanded.field.name,
                            ))
                        })?;
                    }
                }
            }
            let Some(child) = response.child(copy, key) else {
                continue;
            };
            let mut below: Demand<'d> = Demand::default();
            for demanded in asked {
                if demanded.field.selection_set.selections.is_empty() {
                    continue;
                }
                // Each demanded field's subselections below the names its own demand sends it
                // under: one key can hold different fields at different runtime types.
                let sent = match self.agreement {
                    Some(_) => sent_as(demanded),
                    None => Vec::new(),
                };
                below.add(
                    algebra,
                    demanded.region.clone(),
                    sent,
                    demanded.field.selection_set.selections.iter().collect(),
                );
            }
            if below.is_empty() {
                continue;
            }
            self.walk(child, below, sink)
                .map_err(|uncovered| uncovered.under(response.describe(child)))?;
        }
        Ok(())
    }

    /// The configurations of both regions in which the copy is present: each pairing of their
    /// clauses that holds one, `all` of them or the first, and a configuration of the first, for
    /// the explanation.
    fn joint(
        &self,
        copy: CopyId,
        left: &Region,
        right: &Region,
        all: bool,
    ) -> (Region, Option<Clause>) {
        let algebra = self.hypothesis.algebra;
        let mut joint = Vec::new();
        let mut first = None;
        for l in left {
            for r in right {
                let Some(both) = algebra.conjoin(l, r) else {
                    continue;
                };
                if let Some(example) = self.hypothesis.uncovered(copy, &both, &[]) {
                    first.get_or_insert(example);
                    joint.push(both);
                    if !all {
                        return (joint, first);
                    }
                }
            }
        }
        (joint, first)
    }

    /// Do a key's demanded fields make one call wherever two of them can be active together? The
    /// client operation's validity makes this true of it, fields making different calls being at
    /// disjoint runtime types; a requirement assembled from one entity's field sets has it unless
    /// two `@requires` demand one key differently, which `CheckerOptions::check_requires_conflict`
    /// reports.
    fn one_call(&self, copy: CopyId, asked: &[Asked<'_, '_>]) -> bool {
        let algebra = self.hypothesis.algebra;
        // The demanded calls, each with the fields making it: one comparison per field and call.
        let mut calls: Vec<(&Field, Vec<&Asked<'_, '_>>)> = Vec::new();
        for demanded in asked {
            let call = calls.iter_mut().find(|(call, _)| {
                algebra.work.call_comparisons.add(1);
                call.name == demanded.field.name
                    && same_arguments(&call.arguments, &demanded.field.arguments)
            });
            match call {
                Some((_, fields)) => fields.push(demanded),
                None => calls.push((demanded.field, vec![demanded])),
            }
        }
        // Fields making different calls are never active together.
        calls.iter().enumerate().all(|(at, (_, left))| {
            calls[at + 1..].iter().all(|(_, right)| {
                left.iter().all(|left| {
                    right.iter().all(|right| {
                        algebra.work.call_comparisons.add(1);
                        !left.region.iter().any(|l| {
                            right.region.iter().any(|r| {
                                algebra
                                    .conjoin(l, r)
                                    .is_some_and(|both| self.hypothesis.feasible(copy, &both))
                            })
                        })
                    })
                })
            })
        })
    }

    /// Where a key's demanded fields make different calls, the model's `ResponseCoversAt` asks, in
    /// each configuration, for some active demanded field whose call every active fetched field
    /// makes. That is asked configuration by configuration, over the values the key's own literals
    /// tell apart, which only such a demand reaches. The configurations that fail, in the order
    /// they are tried: `all` of them, or the first.
    fn no_call_matches(
        &self,
        copy: CopyId,
        asked: &[Asked<'_, '_>],
        supplied: &[&Contribution],
        all: bool,
    ) -> Vec<Clause> {
        let hypothesis = self.hypothesis;
        let algebra = hypothesis.algebra;
        let clauses = asked
            .iter()
            .flat_map(|demanded| demanded.region.iter())
            .chain(supplied.iter().flat_map(|field| field.guard.iter()));
        // Each variable's values, as the literals on it tell them apart.
        let mut values: Vec<(Var, Vec<Value>)> = Vec::new();
        for clause in clauses {
            for literal in clause {
                let known = match values.iter_mut().find(|(var, _)| *var == literal.var) {
                    Some((_, known)) => known,
                    None => {
                        let all = match &literal.var {
                            Var::Copy(copy) => vec![Value::Types(hypothesis.domain(*copy))],
                            _ => vec![Value::Is(true), Value::Is(false)],
                        };
                        values.push((literal.var.clone(), all));
                        &mut values.last_mut().expect("just pushed").1
                    }
                };
                if let Value::Types(split) = literal.value {
                    *known = known
                        .iter()
                        .flat_map(|value| match value {
                            Value::Types(types) => [
                                algebra.intersect(*types, split),
                                algebra.minus(*types, split),
                            ]
                            .into_iter()
                            .filter(|part| *part != TypeSet::EMPTY)
                            .map(Value::Types)
                            .collect::<Vec<_>>(),
                            value => vec![*value],
                        })
                        .collect();
                }
            }
        }
        let mut assignments: Vec<Clause> = vec![Vec::new()];
        for (var, values) in &values {
            assignments = assignments
                .into_iter()
                .flat_map(|assignment| {
                    values.iter().filter_map(move |value| {
                        algebra.conjoin(
                            &assignment,
                            &[Literal {
                                var: var.clone(),
                                value: *value,
                            }],
                        )
                    })
                })
                .collect();
        }
        let mut failing = Vec::new();
        for assignment in assignments {
            if !hypothesis.feasible(copy, &assignment) {
                continue;
            }
            let active = |region: &Region| {
                region
                    .iter()
                    .any(|clause| algebra.conjoin(clause, &assignment).is_some())
            };
            let demanded: Vec<&Field> = asked
                .iter()
                .filter(|demanded| active(&demanded.region))
                .map(|demanded| demanded.field)
                .collect();
            if demanded.is_empty() {
                continue;
            }
            let fetched: Vec<&&Contribution> = supplied
                .iter()
                .filter(|field| active(&field.guard))
                .collect();
            if !demanded.iter().any(|demanded| {
                fetched
                    .iter()
                    .all(|field| makes_call(field, demanded) && field.result.is_some())
            }) {
                failing.push(assignment);
                if !all {
                    break;
                }
            }
        }
        failing
    }
}

/// Is a contribution the demanded field, with the demanded arguments? The call half of the
/// model's `ProducedField.Matches`.
fn makes_call(field: &Contribution, demanded: &Field) -> bool {
    field.field == demanded.name && same_arguments(&field.arguments, &demanded.arguments)
}

/// A field call as an explanation writes it.
fn render_call(name: &Name, arguments: &[Node<ast::Argument>]) -> String {
    if arguments.is_empty() {
        return name.to_string();
    }
    let arguments: Vec<String> = arguments
        .iter()
        .map(|argument| format!("{}: {}", argument.name, argument.value))
        .collect();
    format!("{name}({})", arguments.join(", "))
}
