//! What an entity fetch sends: the representation its `requires` entries project at the copy it
//! runs on, in each configuration, and what that representation owes the subgraph.
//!
//! Port of `Apollo/Definitions/QueryPlanRequires.lean` and of the representation half of
//! `Apollo/Definitions/QueryPlanSoundness.lean` (`RepresentationCovers`,
//! `FetchSiteRepresentationMet`), decided by `checkRepresentations` in the model's checker.
//!
//! # The projection
//!
//! The router builds an entity's representation with `execute_selection_set`, reading every
//! `requires` entry in document order at the object the fetch runs on. An inline fragment whose
//! type condition admits the object, by supergraph membership or by having none, and whose
//! `@skip`/`@include` conditions hold, projects its selections and merges them into what earlier
//! entries left; a field replaces the whole value an earlier write left under its response key,
//! and a field with no selections copies the whole value it reads. An empty projection sends no
//! representation, and the fetch stays an entity fetch.
//!
//! Every write at one response key at one receiving copy reads the same readable contributions,
//! the ones under that key there, so which write survives never matters, only whether one does.
//! The projection is read once, as three regions of configurations for each *location*, a response
//! key at a receiving copy below the one the fetch runs on, each entry read in the region of the
//! configurations it is reached in, the conjunction of the fragments enclosing it:
//! - *present*: where a write at the location survives. A field adds its region at its key, and
//!   first removes it from every location below its key written earlier in the same scope, a
//!   field's selections and a fragment's being read from an empty start; a fragment's locations are
//!   joined into the enclosing scope's, and remove nothing.
//! - *explicit*: where the surviving write is a field an entry names, with the names of the fields
//!   leading to it, which is what it is sent as. A field removes its region at and below its key;
//!   a fragment's join removes, at each location it writes, what it writes there.
//! - *visited*: where an entry reads the location, with the field the entry names; nothing is ever
//!   removed, a value read and later overwritten having been read all the same.
//!
//! The representation in a configuration is the readable contributions active at the locations
//! present there. It is held as contributions of its own, each readable one narrowed to where its
//! location is present, so that the coverage walk reads it as it reads anything fetched.
//!
//! # What the representation owes
//!
//! At every configuration the fetch runs in at its copy:
//! - every read is of a value the plan fetched there, under the field the entry names, with its
//!   result declared ([`check_inputs_available`]): a surplus read and an overwritten one included;
//! - every explicit selection is carried by the fetch's input rewrites from the key it is read
//!   under to the field it names, and back ([`Agreement`]);
//! - the representation covers what the subgraph demands for the entity's runtime type: its
//!   `__typename`, a resolvable key, and the `@requires` field sets of every field the fetch
//!   selects there under that selection's own conditions, read from every `_entities` fragment
//!   admitting the type; the key may differ from one configuration to another; every demanded
//!   field the representation holds is carried by the rewrites as well. An empty representation
//!   owes nothing where the fetch selects nothing; a non-empty one owes a key even there.
//!
//! Coverage, not equality: a representation may hold more than is demanded, wherever it read it
//! from what was fetched. A value copied whole holds what was fetched under it, whatever its keys,
//! and owes no rename for what nothing demands.
//!
//! # The Boolean boundary
//!
//! A condition is read on the Boolean variables the operation declares, as a condition node's is;
//! that every one the plan reads is declared `Boolean` or `Boolean!` is checked before anything is
//! read (`check_boolean_variables` in the checker). Literal conditions are folded where they are
//! read: `@include(if: false)` and `@skip(if: true)` exclude an entry, the opposite literals are no
//! condition.

use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Selection;

use super::super::query_compare::conditions::literals_for_directives;
use super::super::query_compare::schema_view::SchemaView;
use super::Checker;
use super::ComparisonError;
use super::ENTITIES_FIELD_NAME;
use super::Site;
use super::coverage::Coverage;
use super::coverage::Demand;
use super::coverage::Supplier;
use super::coverage::Supplies;
use super::coverage::describe_at;
use super::coverage::render_clause;
use super::region::Algebra;
use super::region::Clause;
use super::region::Literal;
use super::region::Region;
use super::region::TypeSet;
use super::region::Value;
use super::region::Var;
use super::region::boolean;
use super::requires::check_requires_conflict;
use super::requires::inline_fragment;
use super::requires::key_half;
use super::requires::requires_half;
use super::response::Contribution;
use super::response::CopyId;
use super::response::Response;
use super::response::View;
use super::response::every_object;
use super::selections::admitted_types;
use super::selections::invert_key_renamers;
use super::selections::type_filter_covers;
use super::selections::under_directives;
use super::selections::wrap_non_empty;
use super::subgraph::KeyDirective;
use super::subgraph::Subgraph;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
use crate::query_plan::FetchNode;
use crate::query_plan::requires_selection;
use crate::query_plan::requires_selection::ConditionKind;
use crate::query_plan::requires_selection::ConditionValue;
use crate::schema::ValidFederationSchema;

/// A response key at a receiving copy: where a representation holds a value.
type Location = (CopyId, Name);

//==================================================================================================
// The projection
//==================================================================================================

/// What a fetch's `requires` entries project at the copy it runs on: the model's
/// `requiresProjection` and `Representation`, as regions of the configurations in which each
/// location holds a value, is explicitly selected and is read.
pub(super) struct Projection {
    /// Where a write at each location survives.
    present: IndexMap<Location, Region>,
    /// At each location, where its surviving write is an explicit selection, by the names of the
    /// fields leading to it from the copy the fetch runs on and its own.
    explicit: IndexMap<Location, Vec<(Vec<Name>, Region)>>,
    /// Where each location is read, by the field the entry names there.
    visited: IndexMap<(Location, Name), Region>,
}

/// What one scope has written: an entry list read from an empty start, a field's selections or a
/// fragment's.
#[derive(Default)]
struct Scope {
    present: IndexMap<Location, Region>,
    explicit: IndexMap<Location, Vec<(Vec<Name>, Region)>>,
}

/// Projects a fetch's `requires` entries at the copy it runs on, in `region`, the configurations
/// it runs there in.
pub(super) fn requires_projection(
    algebra: &Algebra,
    schema: &SchemaView<'_>,
    readable: View<'_>,
    copy: CopyId,
    region: &Clause,
    entries: &[requires_selection::Selection],
) -> Projection {
    let mut projector = Projector {
        algebra,
        schema,
        readable,
        visited: IndexMap::default(),
    };
    let mut scope = Scope::default();
    projector.project(copy, &[], region, entries, &mut scope);
    Projection {
        present: scope.present,
        explicit: scope.explicit,
        visited: projector.visited,
    }
}

struct Projector<'p> {
    algebra: &'p Algebra,
    schema: &'p SchemaView<'p>,
    readable: View<'p>,
    visited: IndexMap<(Location, Name), Region>,
}

/// Adds a region to an entry's, as few clauses as hold the same configurations.
fn widen(algebra: &Algebra, region: &mut Region, added: &[Clause]) {
    algebra.work.projection_writes.add(1);
    let domain = every_object(algebra);
    region.extend(added.iter().cloned());
    *region = algebra.simplify(std::mem::take(region), &domain);
}

/// Removes a region from an entry's.
fn narrow(algebra: &Algebra, region: &mut Region, removed: &[Clause]) {
    algebra.work.projection_writes.add(1);
    let domain = every_object(algebra);
    *region = algebra.simplify(
        algebra.subtract_all(std::mem::take(region), removed, &domain),
        &domain,
    );
}

impl Projector<'_> {
    fn response(&self) -> &Response {
        self.readable.response
    }

    /// Reads an entry list at a copy, in a region, into a scope. `names` are the fields leading
    /// to the copy from the one the fetch runs on.
    fn project(
        &mut self,
        copy: CopyId,
        names: &[Name],
        region: &Clause,
        entries: &[requires_selection::Selection],
        into: &mut Scope,
    ) {
        let algebra = self.algebra;
        let reached = std::slice::from_ref(region);
        for entry in entries {
            algebra.work.entry_selections.add(1);
            match entry {
                requires_selection::Selection::Field(field) => {
                    let key = field.alias.clone().unwrap_or_else(|| field.name.clone());
                    let location = (copy, key.clone());
                    widen(
                        algebra,
                        self.visited
                            .entry((location.clone(), field.name.clone()))
                            .or_default(),
                        reached,
                    );
                    // A field replaces the whole value an earlier write in this scope left at its
                    // key: what is below it goes, and so does any explicit selection at or below
                    // it.
                    let child = self.response().child(copy, &key);
                    if let Some(explicit) = into.explicit.get_mut(&location) {
                        for (_, selected) in explicit.iter_mut() {
                            narrow(algebra, selected, reached);
                        }
                    }
                    // Only a key holding a copy has anything below it.
                    if let Some(child) = child {
                        for (at, present) in into.present.iter_mut() {
                            algebra.work.projection_scans.add(1);
                            if self.below(at.0, child) {
                                narrow(algebra, present, reached);
                            }
                        }
                        for (at, explicit) in into.explicit.iter_mut() {
                            algebra.work.projection_scans.add(1);
                            if self.below(at.0, child) {
                                for (_, selected) in explicit.iter_mut() {
                                    narrow(algebra, selected, reached);
                                }
                            }
                        }
                    }
                    widen(
                        algebra,
                        into.present.entry(location.clone()).or_default(),
                        reached,
                    );
                    let mut path = names.to_vec();
                    path.push(field.name.clone());
                    add_explicit(algebra, &mut into.explicit, location, path.clone(), reached);
                    let Some(child) = child else {
                        continue;
                    };
                    if field.selections.is_empty() {
                        // A field with no selections copies the whole value it reads.
                        self.copy_below(child, reached, into);
                    } else {
                        // Nothing earlier is left below the field in this scope, so what its
                        // selections write is added as it is.
                        let mut below = Scope::default();
                        self.project(child, &path, region, &field.selections, &mut below);
                        for (at, present) in below.present {
                            widen(algebra, into.present.entry(at).or_default(), &present);
                        }
                        for (at, explicit) in below.explicit {
                            for (path, selected) in explicit {
                                add_explicit(
                                    algebra,
                                    &mut into.explicit,
                                    at.clone(),
                                    path,
                                    &selected,
                                );
                            }
                        }
                    }
                }
                requires_selection::Selection::InlineFragment(fragment) => {
                    algebra
                        .work
                        .entry_literals
                        .add(fragment.conditions.len() as u64);
                    let Some(admitted) = self.admission(copy, fragment) else {
                        continue;
                    };
                    let Some(region) = algebra.conjoin(region, &admitted) else {
                        continue;
                    };
                    let mut later = Scope::default();
                    self.project(copy, names, &region, &fragment.selections, &mut later);
                    // A fragment merges: an earlier explicit selection where the fragment writes
                    // the same location is that write's now, and nothing else earlier goes.
                    for (at, present) in later.present {
                        if let Some(explicit) = into.explicit.get_mut(&at) {
                            for (_, selected) in explicit.iter_mut() {
                                narrow(algebra, selected, &present);
                            }
                        }
                        widen(algebra, into.present.entry(at).or_default(), &present);
                    }
                    for (at, explicit) in later.explicit {
                        for (path, selected) in explicit {
                            add_explicit(algebra, &mut into.explicit, at.clone(), path, &selected);
                        }
                    }
                }
            }
        }
    }

    /// Is a copy the given one or below it?
    fn below(&self, copy: CopyId, ancestor: CopyId) -> bool {
        let mut current = copy;
        loop {
            if current == ancestor {
                return true;
            }
            match self.response().parent(current) {
                Some((parent, _)) => current = parent,
                None => return false,
            }
        }
    }

    /// Every readable location at and below a copy, present in `reached`: what a field with no
    /// selections copies.
    fn copy_below(&self, copy: CopyId, reached: &[Clause], into: &mut Scope) {
        let response = self.response();
        for key in response.keys_at(copy) {
            if self.readable.fields_at(copy, key).next().is_none() {
                continue;
            }
            self.algebra.work.copied_locations.add(1);
            widen(
                self.algebra,
                into.present.entry((copy, key.clone())).or_default(),
                reached,
            );
            if let Some(child) = response.child(copy, key) {
                self.copy_below(child, reached, into);
            }
        }
    }

    /// The configurations a fragment admits the copy in, its literal conditions folded: `None`
    /// where it admits it in none, which `@include(if: false)`, `@skip(if: true)` and an
    /// `@include` and a `@skip` on one variable make, as does a type condition no object the copy
    /// can be has. The model's `requiresFragmentAllows`, a type condition read in the supergraph.
    fn admission(
        &self,
        copy: CopyId,
        fragment: &requires_selection::InlineFragment,
    ) -> Option<Clause> {
        let algebra = self.algebra;
        let mut literals = Vec::new();
        for condition in &fragment.conditions {
            let include = condition.kind == ConditionKind::Include;
            match &condition.if_ {
                ConditionValue::Boolean(value) if *value == include => {}
                ConditionValue::Boolean(_) => return None,
                ConditionValue::Variable(variable) => literals.push(Literal {
                    var: Var::Boolean(variable.clone()),
                    value: Value::Is(include),
                }),
            }
        }
        let clause = algebra.clause(literals)?;
        let Some(type_condition) = &fragment.type_condition else {
            return Some(clause);
        };
        let mut admitted = vec![type_condition.clone()];
        admitted.extend(self.schema.possible_types(type_condition).iter().cloned());
        let literal = self
            .response()
            .type_literal(algebra, copy, algebra.objects(&admitted))?;
        algebra.conjoin(&clause, &literal)
    }
}

fn add_explicit(
    algebra: &Algebra,
    explicit: &mut IndexMap<Location, Vec<(Vec<Name>, Region)>>,
    location: Location,
    path: Vec<Name>,
    added: &[Clause],
) {
    let at = explicit.entry(location).or_default();
    match at.iter_mut().find(|(known, _)| *known == path) {
        Some((_, selected)) => widen(algebra, selected, added),
        None => {
            algebra.work.projection_writes.add(1);
            at.push((path, added.to_vec()));
        }
    }
}

impl Projection {
    /// The representation as contributions: each readable contribution at a present location,
    /// narrowed to where the location is present.
    pub(super) fn held(&self, algebra: &Algebra, readable: View<'_>) -> Vec<Contribution> {
        let domain = every_object(algebra);
        let mut held = Vec::new();
        for ((copy, key), present) in &self.present {
            if present.is_empty() {
                continue;
            }
            for field in readable.fields_at(*copy, key) {
                let guard = algebra.simplify(algebra.product(&field.guard, present), &domain);
                if guard.is_empty() {
                    continue;
                }
                algebra.work.held_contributions.add(1);
                algebra
                    .work
                    .held_guard_literals
                    .add(guard.iter().map(|clause| clause.len() as u64).sum());
                held.push(Contribution {
                    copy: *copy,
                    key: key.clone(),
                    producer: field.producer,
                    subgraph_parent: field.subgraph_parent.clone(),
                    field: field.field.clone(),
                    arguments: field.arguments.clone(),
                    guard,
                    result: field.result,
                });
            }
        }
        held
    }

    /// Where a representation is sent at the copy the fetch runs on: some location of it there is
    /// present. A location there is written by a field, so it is read, and where it is read its
    /// value was fetched, which [`check_inputs_available`] checks first.
    fn sends(&self, algebra: &Algebra, copy: CopyId) -> Region {
        let domain = every_object(algebra);
        algebra.simplify(
            self.present
                .iter()
                .filter(|((at, _), _)| *at == copy)
                .flat_map(|(_, present)| present.iter().cloned())
                .collect(),
            &domain,
        )
    }
}

//==================================================================================================
// What the representation owes
//==================================================================================================

/// Is everything the projection reads readable where it reads it, under the field the entry
/// names, with its result declared? The model's `RequiresInputsAvailable`: every read, a surplus
/// one and one a later write overwrites included. Positions come from what was fetched before the
/// fetch, values from what it can read, so an unreadable value cannot erase the read it is owed at.
pub(super) fn check_inputs_available(
    site: &Site<'_>,
    projection: &Projection,
) -> Result<(), ComparisonError> {
    let hypothesis = site.hypothesis;
    let algebra = hypothesis.algebra;
    let response = site.readable.response;
    for (((copy, key), name), region) in &projection.visited {
        let supplies = site.supplies.at(hypothesis, *copy, key);
        for clause in region {
            if let Some(example) = hypothesis.uncovered(*copy, clause, &supplies) {
                return Err(ComparisonError::new(format!(
                    "fetch to {}: the representation reads `{key}` at {}, which the plan has not \
                     fetched there",
                    site.fetch.subgraph_name,
                    describe_at(hypothesis, *copy, example),
                )));
            }
        }
        for field in site.readable.fields_at(*copy, key) {
            if field.field == *name && field.result.is_some() {
                continue;
            }
            for clause in region {
                for guard in &field.guard {
                    let Some(both) = algebra.conjoin(clause, guard) else {
                        continue;
                    };
                    let Some(example) = hypothesis.uncovered(*copy, &both, &[]) else {
                        continue;
                    };
                    let at = describe_at(hypothesis, *copy, example);
                    return Err(ComparisonError::new(if field.field != *name {
                        format!(
                            "fetch to {}: the representation reads `{key}` at {at} as `{name}`, \
                             where the plan fetched `{}` there",
                            site.fetch.subgraph_name, field.field,
                        )
                    } else {
                        format!(
                            "fetch to {}: the representation reads `{key}` at {at}, and `{}.{}` \
                             in `{}` has a missing declaration or an unjustified correspondence",
                            site.fetch.subgraph_name,
                            field.subgraph_parent,
                            field.field,
                            response.producer(field.producer),
                        )
                    }));
                }
            }
        }
    }
    Ok(())
}

/// The fetch's input rewrites read against what its representation sends: the model's
/// `requireAgreesWithRewrites` over `representationEntry`, one location at a time.
///
/// The model reads the surviving explicit selections and the demanded fields a representation
/// holds as one entry, each nested value under an inline fragment on the runtime type of the copy
/// it is, and asks that the rewrites, applied forwards, carry it as read to it as sent, and,
/// inverted, back. Renaming moves keys and nothing else, so the two readings have one shape and
/// are compared key for key: the entry agrees exactly when every location in it does. A location
/// agrees where the renamers reaching it, applied in order to the keys it is read under from the
/// copy the fetch runs on, end at the name of the field it is sent as, and, inverted from the last
/// on the names it is sent as, end at the key it is read under. A renamer with a `typenameEquals`
/// before its last key renames only where the object holding that key is of a type the filter
/// admits, so each such renamer splits the configurations by that copy's runtime type; a filter
/// before an earlier key is passed over, as `rename_key_under` passes it over, and an index is. A
/// path renames the key it ends at: one ending at an index or a filter renames nothing, as
/// `invert_key_renamers` finds no key there.
pub(super) struct Agreement<'a> {
    supergraph: &'a ValidFederationSchema,
    /// The copy the fetch runs on, where every location's keys start.
    anchor: CopyId,
    /// The key renamers, forwards in order, and each inverted, from the last.
    forwards: Vec<Renamer>,
    backwards: Vec<Renamer>,
}

/// One key renamer as a path of keys, each with the type filters written before it.
struct Renamer {
    steps: Vec<(Vec<Name>, Name)>,
    to: Name,
}

impl<'a> Agreement<'a> {
    pub(super) fn new(
        supergraph: &'a ValidFederationSchema,
        rewrites: &[Arc<FetchDataRewrite>],
        anchor: CopyId,
    ) -> Self {
        Agreement {
            supergraph,
            anchor,
            forwards: Self::renamers(rewrites, false),
            backwards: Self::renamers(rewrites, true),
        }
    }

    /// The key renamers as paths of keys, forwards in order, or each inverted, from the last.
    fn renamers(rewrites: &[Arc<FetchDataRewrite>], inverted: bool) -> Vec<Renamer> {
        let mut renamers = Vec::new();
        for rewrite in rewrites {
            let FetchDataRewrite::KeyRenamer(renamer) = rewrite.as_ref() else {
                continue;
            };
            // A path ending anywhere but at a key renames nothing.
            if !matches!(renamer.path.last(), Some(FetchDataPathElement::Key(..))) {
                continue;
            }
            let mut steps: Vec<(Vec<Name>, Name)> = Vec::new();
            let mut filters: Vec<Name> = Vec::new();
            let mut followed = true;
            for element in &renamer.path {
                match element {
                    FetchDataPathElement::Key(key, _) => {
                        steps.push((std::mem::take(&mut filters), key.clone()));
                    }
                    FetchDataPathElement::TypenameEquals(type_name) => {
                        filters.push(type_name.clone())
                    }
                    FetchDataPathElement::AnyIndex(_) => {}
                    FetchDataPathElement::Parent => followed = false,
                }
            }
            // `Parent` moves up, so a path through one is not followed downwards.
            if !followed {
                continue;
            }
            let (to, steps) = if inverted {
                let last = steps.len() - 1;
                let from = std::mem::replace(&mut steps[last].1, renamer.rename_key_to.clone());
                (from, steps)
            } else {
                (renamer.rename_key_to.clone(), steps)
            };
            renamers.push(Renamer { steps, to });
        }
        if inverted {
            renamers.reverse();
        }
        renamers
    }

    /// The keys a location ends with once the renamers are applied, each with the configurations
    /// it ends with it in. `holders` are the copies holding each key, from the anchor down.
    fn renamed(
        &self,
        algebra: &Algebra,
        response: &Response,
        holders: &[CopyId],
        keys: Vec<Name>,
        inverted: bool,
    ) -> Vec<(Name, Clause)> {
        let mut states: Vec<(Vec<Name>, Clause)> = vec![(keys, Vec::new())];
        let renamers = if inverted {
            &self.backwards
        } else {
            &self.forwards
        };
        for renamer in renamers {
            let depth = renamer.steps.len();
            if depth > holders.len() {
                continue;
            }
            let mut next = Vec::with_capacity(states.len());
            for (keys, clause) in states {
                algebra.work.agreement_states.add(1);
                let reaches = renamer
                    .steps
                    .iter()
                    .zip(&keys)
                    .all(|((_, step), key)| step == key);
                if !reaches {
                    next.push((keys, clause));
                    continue;
                }
                let mut renamed = keys.clone();
                renamed[depth - 1] = renamer.to.clone();
                let filters = &renamer.steps[depth - 1].0;
                let Some(admitted) = admitted_types(self.supergraph, filters) else {
                    next.push((renamed, clause));
                    continue;
                };
                // The filter splits the configurations by the runtime type of the object
                // holding the key.
                let admitted = algebra.objects(&admitted);
                let holder = holders[depth - 1];
                let other = algebra.minus(algebra.every_object(), admitted);
                for (keys, types) in [(renamed, admitted), (keys, other)] {
                    if let Some(split) = response
                        .type_literal(algebra, holder, types)
                        .and_then(|literal| algebra.conjoin(&clause, &literal))
                    {
                        next.push((keys, split));
                    }
                }
            }
            states = next;
        }
        states
            .into_iter()
            .map(|(keys, clause)| (keys.last().cloned().expect("a location has a key"), clause))
            .collect()
    }

    /// The configurations in which the rewrites carry the location `key` at `copy` to the field
    /// it is sent as, the last of `names`, and back, `names` being the fields the location's keys
    /// are sent as from the anchor down.
    pub(super) fn region(
        &self,
        algebra: &Algebra,
        response: &Response,
        copy: CopyId,
        key: &Name,
        names: &[Name],
    ) -> Region {
        let chain = response.ancestors(copy);
        let Some(start) = chain.iter().position(|ancestor| *ancestor == self.anchor) else {
            return Vec::new();
        };
        let holders = &chain[start..];
        let mut read: Vec<Name> = holders[1..]
            .iter()
            .filter_map(|copy| response.parent(*copy).map(|(_, key)| key.clone()))
            .collect();
        read.push(key.clone());
        if read.len() != names.len() {
            return Vec::new();
        }
        let sent = names.last().expect("a location has a name");
        let forwards: Region = self
            .renamed(algebra, response, holders, read, false)
            .into_iter()
            .filter(|(renamed, _)| renamed == sent)
            .map(|(_, clause)| clause)
            .collect();
        let backwards: Region = self
            .renamed(algebra, response, holders, names.to_vec(), true)
            .into_iter()
            .filter(|(renamed, _)| renamed == key)
            .map(|(_, clause)| clause)
            .collect();
        let domain = every_object(algebra);
        algebra.simplify(algebra.product(&forwards, &backwards), &domain)
    }
}

/// Is every surviving explicit selection carried by the fetch's input rewrites from the key it is
/// read under to the field it names, and back, wherever it survives? The half of
/// `requireAgreesWithRewrites` the entries owe whatever is demanded; the demanded half is the
/// coverage walk's.
pub(super) fn check_explicit_agreement(
    site: &Site<'_>,
    projection: &Projection,
    agreement: &Agreement<'_>,
) -> Result<(), ComparisonError> {
    let hypothesis = site.hypothesis;
    let algebra = hypothesis.algebra;
    let response = site.readable.response;
    for ((copy, key), explicit) in &projection.explicit {
        for (names, region) in explicit {
            if region.is_empty() {
                continue;
            }
            let agrees = agreement.region(algebra, response, *copy, key, names);
            for clause in region {
                if let Some(example) = hypothesis.uncovered(*copy, clause, &agrees) {
                    return Err(ComparisonError::new(format!(
                        "fetch to {}: the representation sends `{key}` at {} as `{}`, and the \
                         fetch's input rewrites do not carry the one to the other",
                        site.fetch.subgraph_name,
                        describe_at(hypothesis, *copy, example),
                        names.last().expect("a location has a name"),
                    )));
                }
            }
        }
    }
    Ok(())
}

//==================================================================================================
// The `_entities` body
//==================================================================================================

/// The fragments under an entity fetch's `_entities`: every selection there is an inline fragment
/// carrying a type condition, which the model's `entityOperationWellFormed` asks and
/// `compute_response_shape_for_entity_fetch_operation` and `get_fragment_type_condition` check. A
/// `requires` entry may lack a type condition; an `_entities` fragment may not.
pub(super) fn entity_fragments<'b>(
    fetch: &FetchNode,
    entity_body: &'b [Selection],
) -> Result<Vec<(&'b Name, &'b Node<InlineFragment>)>, ComparisonError> {
    entity_body
        .iter()
        .map(|selection| {
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
            Ok((entity_type, fragment))
        })
        .collect()
}

/// What the fetch selects on one subgraph entity type: every `_entities` fragment admitting it,
/// read together, each kept under its own directives. The model's `FetchNode.entitySelectionsAt`.
fn entity_selections_at(
    subgraph: &Subgraph<'_>,
    entity_type: &Name,
    fragments: &[(&Name, &Node<InlineFragment>)],
) -> Vec<Selection> {
    let mut selections = Vec::new();
    for (type_condition, fragment) in fragments {
        if subgraph.type_includes_object(type_condition, entity_type) {
            selections.extend(under_directives(
                &fragment.directives,
                entity_type,
                fragment.selection_set.selections.clone(),
            ));
        }
    }
    selections
}

/// The resolvable keys an entity type can be resolved from: its own and those of every
/// `_entities` fragment's type admitting it, an abstract entity's key identifying each object it
/// admits. The model's `FetchNode.entityKeysAt`.
fn entity_keys_at(
    subgraph: &Subgraph<'_>,
    entity_type: &Name,
    fragments: &[(&Name, &Node<InlineFragment>)],
) -> Result<Vec<KeyDirective>, ComparisonError> {
    let mut types = vec![entity_type];
    for (type_condition, _) in fragments {
        if !types.contains(type_condition)
            && subgraph.type_includes_object(type_condition, entity_type)
        {
            types.push(type_condition);
        }
    }
    let mut keys: Vec<KeyDirective> = Vec::new();
    for type_name in types {
        for key in subgraph.keys(type_name)? {
            if key.resolvable && !keys.iter().any(|known| known.fields == key.fields) {
                keys.push(key);
            }
        }
    }
    Ok(keys)
}

/// Where the fetch selects anything on an entity type: some field of an admitting fragment is
/// active there, under its directives and those of the fragments around it.
fn active_region(
    algebra: &Algebra,
    subgraph: &Subgraph<'_>,
    entity_type: &Name,
    selections: &[Selection],
    inherited: &Clause,
    active: &mut Region,
) {
    for selection in selections {
        let (directives, below) = match selection {
            Selection::Field(field) => (&field.directives, None),
            Selection::InlineFragment(fragment) => {
                if fragment
                    .type_condition
                    .as_ref()
                    .is_some_and(|type_condition| {
                        !subgraph.type_includes_object(type_condition, entity_type)
                    })
                {
                    continue;
                }
                (
                    &fragment.directives,
                    Some(&fragment.selection_set.selections),
                )
            }
            // Spreads are inlined before a fetch's selections reach here.
            Selection::FragmentSpread(_) => continue,
        };
        let Some(literals) = literals_for_directives(directives) else {
            continue;
        };
        let Some(clause) = algebra
            .clause(literals.iter().map(boolean))
            .and_then(|clause| algebra.conjoin(inherited, &clause))
        else {
            continue;
        };
        match below {
            None => active.push(clause),
            Some(below) => active_region(algebra, subgraph, entity_type, below, &clause, active),
        }
    }
}

//==================================================================================================
// Coverage, a key chosen per configuration
//==================================================================================================

impl Checker<'_> {
    /// Does the representation cover what the subgraph demands, wherever the fetch runs and a
    /// representation is owed? The model's `RepresentationCovers` at every copy and configuration
    /// of `FetchSiteRepresentationMet`, decided per runtime type of the copy the fetch runs on,
    /// each read at its subgraph entity type: the demand there is the key half at the runtime type
    /// and the `@requires` half at the entity type, rewrites inverted, and some resolvable key
    /// covers it in each configuration.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn check_representations(
        &self,
        fetch: &FetchNode,
        subgraph: &Subgraph<'_>,
        fragments: &[(&Name, &Node<InlineFragment>)],
        entity_types: &[(Name, Vec<Name>)],
        projection: &Projection,
        held: View<'_>,
        agreement: &Agreement<'_>,
        site: &Site<'_>,
    ) -> Result<(), ComparisonError> {
        let hypothesis = site.hypothesis;
        let algebra = hypothesis.algebra;
        let response = site.readable.response;
        let copy = site.position.copy;
        let domain = every_object(algebra);
        let runs: Region = vec![site.region.clone()];
        let sends = projection.sends(algebra, copy);
        let supplied = Supplies::new(held);
        let mut interpreted = TypeSet::EMPTY;
        for (entity_type, objects) in entity_types {
            let objects = algebra.objects(objects);
            interpreted = algebra.union(interpreted, objects);
            let Some(at_types) = response.type_literal(algebra, copy, objects) else {
                continue;
            };
            let selections = entity_selections_at(subgraph, entity_type, fragments);
            let requires =
                requires_half(self.supergraph_schema, subgraph, entity_type, &selections)?;
            if self.options.check_requires_conflict {
                check_requires_conflict(self.supergraph_schema, &requires).map_err(|e| {
                    ComparisonError::new(format!(
                        "fetch to {}: entity case `{entity_type}`: {}",
                        fetch.subgraph_name,
                        e.description()
                    ))
                })?;
            }
            // A representation is owed where one is sent, and where the fetch selects anything.
            let mut owed = sends.clone();
            active_region(
                algebra,
                subgraph,
                entity_type,
                &selections,
                &Vec::new(),
                &mut owed,
            );
            let owed = algebra.simplify(
                algebra.product(
                    &algebra.product(&runs, &owed),
                    std::slice::from_ref(&at_types),
                ),
                &domain,
            );
            if owed.is_empty() {
                continue;
            }
            let keys = entity_keys_at(subgraph, entity_type, fragments)?;
            for object in algebra.names(hypothesis.allowed(copy, &owed)) {
                let Some(literal) =
                    response.type_literal(algebra, copy, algebra.objects([&object]))
                else {
                    continue;
                };
                let owed = algebra.simplify(
                    algebra.product(&owed, std::slice::from_ref(&literal)),
                    &domain,
                );
                self.representation_covers(
                    fetch,
                    entity_type,
                    &object,
                    &requires,
                    &keys,
                    owed,
                    agreement,
                    &supplied,
                    site,
                )?;
            }
        }
        // A runtime type the subgraph resolves no entity of is sent no representation.
        let uninterpreted = algebra.minus(hypothesis.allowed(copy, &runs), interpreted);
        for object in algebra.names(uninterpreted) {
            let Some(literal) = response.type_literal(algebra, copy, algebra.objects([&object]))
            else {
                continue;
            };
            for clause in algebra.product(&algebra.product(&runs, &sends), &[literal]) {
                if let Some(example) = hypothesis.uncovered(copy, &clause, &[]) {
                    return Err(ComparisonError::new(format!(
                        "fetch to {}: the representation sent at {} is of a `{object}`, which \
                         the subgraph resolves no entity of",
                        fetch.subgraph_name,
                        describe_at(hypothesis, copy, example),
                    )));
                }
            }
        }
        Ok(())
    }

    /// Does the representation cover the demand at one runtime type, through a resolvable key
    /// chosen in each configuration of `owed`? A key covering every configuration is chosen
    /// outright; otherwise every key's failures are collected and their configurations intersected,
    /// and the representation covers the demand where no configuration fails for every key.
    #[allow(clippy::too_many_arguments)]
    fn representation_covers(
        &self,
        fetch: &FetchNode,
        entity_type: &Name,
        object: &Name,
        requires: &[Selection],
        keys: &[KeyDirective],
        owed: Region,
        agreement: &Agreement<'_>,
        supplied: &Supplies<'_>,
        site: &Site<'_>,
    ) -> Result<(), ComparisonError> {
        let hypothesis = site.hypothesis;
        let algebra = hypothesis.algebra;
        let response = site.readable.response;
        let copy = site.position.copy;
        let no_fragments = Default::default();
        let coverage = Coverage {
            schema: &self.schema_view,
            fragments: &no_fragments,
            hypothesis,
            supplied,
            supplier: Supplier::Representation,
            agreement: Some(agreement),
        };
        let at = response.describe(copy);
        // The demand of each key that applies at the runtime type, what must be read for it once
        // the rewrites are inverted.
        let mut demands: Vec<(&KeyDirective, Vec<Selection>)> = Vec::new();
        let mut first: Option<String> = None;
        for key in keys {
            algebra.work.key_candidates.add(1);
            // A key that does not apply at the runtime type is no key of it.
            let Ok(mut demand) = key_half(self.supergraph_schema, object, &key.fields) else {
                continue;
            };
            demand.extend(requires.iter().cloned());
            let required = invert_key_renamers(
                self.supergraph_schema,
                type_filter_covers,
                &fetch.input_rewrites,
                wrap_non_empty(
                    |selections| inline_fragment(object.clone(), selections),
                    demand,
                ),
            );
            match coverage.covers(
                copy,
                Demand::of(algebra, owed.clone(), required.iter().collect()),
            ) {
                Ok(()) => return Ok(()),
                Err(uncovered) => {
                    first.get_or_insert_with(|| {
                        format!(
                            "fetch to {}: the representation sent at {at} does not cover what the \
                             subgraph demands of a `{object}` through @key({}):\n{uncovered}",
                            fetch.subgraph_name, key.fields
                        )
                    });
                }
            }
            demands.push((key, required));
        }
        if demands.is_empty() {
            let example = owed
                .iter()
                .find_map(|clause| hypothesis.uncovered(copy, clause, &[]));
            let Some(example) = example else {
                return Ok(());
            };
            return Err(ComparisonError::new(format!(
                "fetch to {}: the representation sent at {} is owed a resolvable `@key` of \
                 `{entity_type}` that applies to a `{object}`, and the subgraph declares none",
                fetch.subgraph_name,
                describe_at(hypothesis, copy, example),
            )));
        }
        if demands.len() == 1 {
            return Err(ComparisonError::new(first.expect("the one key failed")));
        }
        // No one key covers every configuration: some key has to in each.
        let mut left: Region = owed;
        for (key, required) in &demands {
            let failing = coverage
                .failures(
                    copy,
                    Demand::of(algebra, left.clone(), required.iter().collect()),
                )
                .map_err(|uncovered| {
                    ComparisonError::new(format!(
                        "fetch to {}: what the subgraph demands of a `{object}` through @key({}) \
                         cannot be read:\n{uncovered}",
                        fetch.subgraph_name, key.fields
                    ))
                })?;
            algebra.work.key_failure_clauses.add(failing.len() as u64);
            let completion = hypothesis.completion();
            left = algebra.simplify(
                algebra
                    .product(&left, &failing)
                    .into_iter()
                    .filter(|clause| hypothesis.feasible_below(copy, clause))
                    .collect(),
                &completion,
            );
            if left.is_empty() {
                return Ok(());
            }
        }
        // Every key fails in the configuration explained, each explained in it.
        let configuration = &left[0];
        let explained: Vec<String> = demands
            .iter()
            .map(|(key, required)| {
                let why = coverage
                    .covers(
                        copy,
                        Demand::of(
                            algebra,
                            vec![configuration.clone()],
                            required.iter().collect(),
                        ),
                    )
                    .err()
                    .map(|uncovered| uncovered.to_string())
                    .unwrap_or_default();
                format!("  @key({}):\n{why}", key.fields)
            })
            .collect();
        Err(ComparisonError::new(format!(
            "fetch to {}: the representation sent at {at} covers what the subgraph demands of a \
             `{object}` through none of its resolvable keys{}:\n{}",
            fetch.subgraph_name,
            render_clause(algebra, response, configuration),
            explained.join("\n"),
        )))
    }
}

//==================================================================================================
// The variables a plan's conditions read
//==================================================================================================

/// The Boolean variables a `requires` entry list's conditions name, at any depth, read before any
/// literal is folded: an entry a literal excludes, or an `@include` and a `@skip` on one variable,
/// still names its variables. The model's `requiresVariables`.
pub(super) fn requires_variables<'e>(
    algebra: &Algebra,
    entries: &'e [requires_selection::Selection],
    out: &mut Vec<&'e Name>,
) {
    for entry in entries {
        match entry {
            requires_selection::Selection::Field(field) => {
                requires_variables(algebra, &field.selections, out)
            }
            requires_selection::Selection::InlineFragment(fragment) => {
                for condition in &fragment.conditions {
                    algebra.work.boolean_references.add(1);
                    if let ConditionValue::Variable(variable) = &condition.if_ {
                        out.push(variable);
                    }
                }
                requires_variables(algebra, &fragment.selections, out);
            }
        }
    }
}
