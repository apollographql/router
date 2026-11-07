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
//! The projection is read once, as two regions of configurations for each *location*, a response
//! key at a receiving copy below the one the fetch runs on, each entry read in the region of the
//! configurations it is reached in, the conjunction of the fragments enclosing it:
//! - *present*: where a write at the location survives. A field adds its region at its key, and
//!   first removes it from every location below its key written earlier in the same scope, a
//!   field's selections and a fragment's being read from an empty start; a fragment's locations are
//!   joined into the enclosing scope's, and remove nothing.
//! - *visited*: where an entry reads the location, with the field the entry names; nothing is ever
//!   removed, a value read and later overwritten having been read all the same.
//!
//! The representation in a configuration is the readable contributions active at the locations
//! present there. It is held as contributions of its own, each readable one narrowed to where its
//! location is present, so that the coverage walk reads it as it reads anything fetched.
//!
//! # What the input rewrites send
//!
//! The router applies the fetch's input rewrites to each representation it projected, one after
//! another, each to what the ones before it left (`apply_rewrites`). A key renamer selects the
//! objects its path's parent reaches and, at each one holding its last key, removes that key and
//! inserts its value under the new name, replacing whatever the object held there; at an object
//! not holding the key it does nothing. A `TypenameEquals` on the path tests the object it stands
//! before by the `__typename` the representation holds there at that moment, and admits it where
//! that is the type or a subtype, or where there is none (`is_object_of_type`). An index takes a
//! list's elements and selects nothing at an object, and a renamer acts at an object or at each
//! object of a list. A value setter writes at an object holding its key, never into a list's
//! elements, its value replacing what the key held; one writing `__typename` changes what later
//! filters read. [`SentRepresentation`] follows the rewrites so, per configuration, over the locations the
//! representation holds: where each one now sits, whether it survives, and for one sitting under
//! `__typename` what it holds there. A renamer moves only a location that is held, since it reaches
//! a key through the keys above it, and a move replaces what it lands on, so one surviving location
//! at most is sent under one path in a configuration; a value a setter wrote is read as the type
//! name under `__typename` and as no field's value under any other name. Two readings of a rewrite
//! path are the router's only in part, and no planner writes them on an input rewrite: a key's own
//! type conditions are not read, and a value setter on any key but `__typename` is passed over.
//!
//! This departs from the model's `requireAgreesWithRewrites`, which rewrites an entry's selections
//! by syntax, forwards as read and inverted as sent, wherever a renamer's path reaches, whether or
//! not the representation holds the key there: a key renamed by case, which the router renames at
//! the cases that hold it alone, has no inverse there.
//!
//! # What the representation owes
//!
//! At every configuration the fetch runs in at its copy:
//! - every read is of a value the plan fetched there, under the field the entry names, with its
//!   result declared ([`check_inputs_available`]): a surplus read and an overwritten one included;
//! - the representation as the rewrites send it covers what the subgraph demands for the entity's
//!   runtime type: its `__typename`, a resolvable key, and the `@requires` field sets of every field
//!   the fetch selects there under that selection's own conditions, read from every `_entities`
//!   fragment admitting the type; the key may differ from one configuration to another. Each
//!   demanded field is read at the location sent under its names, and owes what a fetched one
//!   owes there. An empty representation owes nothing where the fetch selects nothing; a non-empty
//!   one owes a key even there.
//!
//! Coverage, not equality: a representation may hold more than is demanded, wherever it read it
//! from what was fetched, and send it under any name nothing demands, renamed or not.
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
use super::region::everything;
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
use crate::schema::position::INTROSPECTION_TYPENAME_FIELD_NAME;

/// A response key at a receiving copy: where a representation holds a value.
type Location = (CopyId, Name);

//==================================================================================================
// The projection
//==================================================================================================

/// What a fetch's `requires` entries project at the copy it runs on: the model's
/// `requiresProjection` and `Representation`, as regions of the configurations in which each
/// location holds a value and is read.
pub(super) struct Projection {
    /// Where a write at each location survives.
    present: IndexMap<Location, Region>,
    /// Where each location is read, by the field the entry names there.
    visited: IndexMap<(Location, Name), Region>,
}

/// Where a write at each location survives in what one scope has written: an entry list read from
/// an empty start, a field's selections or a fragment's.
type Scope = IndexMap<Location, Region>;

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
    projector.project(copy, region, entries, &mut scope);
    Projection {
        present: scope,
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

    /// Reads an entry list at a copy, in a region, into a scope.
    fn project(
        &mut self,
        copy: CopyId,
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
                    // key: what is below it goes. Only a key holding a copy has anything below it.
                    let child = self.response().child(copy, &key);
                    if let Some(child) = child {
                        for (at, present) in into.iter_mut() {
                            algebra.work.projection_scans.add(1);
                            if self.below(at.0, child) {
                                narrow(algebra, present, reached);
                            }
                        }
                    }
                    widen(algebra, into.entry(location).or_default(), reached);
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
                        self.project(child, region, &field.selections, &mut below);
                        for (at, present) in below {
                            widen(algebra, into.entry(at).or_default(), &present);
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
                    self.project(copy, &region, &fragment.selections, &mut later);
                    // A fragment merges: nothing earlier goes.
                    for (at, present) in later {
                        widen(algebra, into.entry(at).or_default(), &present);
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
                into.entry((copy, key.clone())).or_default(),
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

//==================================================================================================
// What the input rewrites send
//==================================================================================================

/// A rewrite path as the router follows it: each key with the elements written before it. A key's
/// own type conditions are not read.
type Steps = Vec<Step>;

/// One key of a rewrite path and what the path does at the value before it: the object the key's
/// holder is, or the list of them its parent's key holds.
struct Step {
    before: Vec<Element>,
    key: Name,
}

/// What a rewrite path does at a value before its next key.
enum Element {
    /// A `Flatten`: an array's elements, and nothing at an object.
    Index,
    /// A `Fragment`: the objects of an array, at any depth, or the object, its `__typename`
    /// admitting the type.
    Filter(Name),
}

impl Step {
    fn filters(&self) -> Vec<Name> {
        self.before
            .iter()
            .filter_map(|element| match element {
                Element::Filter(name) => Some(name.clone()),
                Element::Index => None,
            })
            .collect()
    }
}

/// What the rewrite does where its path ends, which decides what value it can act at.
#[derive(Clone, Copy)]
enum Act {
    /// A key renamer, which renames at an object or at each object of an array.
    Rename,
    /// A value setter, which writes at an object alone.
    Set,
}

/// Does a path select anything at a value `depth` lists deep, through `before`, and, where `act` is
/// what the rewrite does there, can it act at what it selects? A `Flatten` takes an array's
/// elements and selects nothing at an object; a `Fragment` reaches the objects of an array at any
/// depth (`iterate_path_mut`).
fn selects(before: &[Element], depth: usize, act: Option<Act>) -> bool {
    let mut depth = depth;
    for element in before {
        match element {
            Element::Index if depth == 0 => return false,
            Element::Index => depth -= 1,
            Element::Filter(_) => depth = 0,
        }
    }
    match act {
        None => true,
        Some(Act::Rename) => depth <= 1,
        Some(Act::Set) => depth == 0,
    }
}

/// How many lists deep a field's declared type is, `None` where the supergraph declares no such
/// field.
fn list_depth(supergraph: &ValidFederationSchema, parent: &Name, field: &Name) -> Option<usize> {
    if *field == *INTROSPECTION_TYPENAME_FIELD_NAME {
        return Some(0);
    }
    let definition = supergraph.schema().type_field(parent, field).ok()?;
    let mut ty = &definition.ty;
    let mut depth = 0;
    while ty.is_list() {
        depth += 1;
        ty = ty.item_type();
    }
    Some(depth)
}

/// An input rewrite that moves a key or changes what a later filter reads.
enum Rewrite {
    /// A key renamer: the key its steps end at, moved to `to`.
    Rename { steps: Steps, to: Name },
    /// A value setter writing `__typename`.
    Typename { steps: Steps, value: Written },
}

/// What a value setter wrote under `__typename`.
#[derive(Clone, PartialEq)]
enum Written {
    /// A string, which a filter reads as a type's name.
    Name(String),
    /// Anything else, which every filter admits (`is_object_of_type` reads no string there).
    Other,
}

impl Rewrite {
    /// One input rewrite as the router applies it, `None` for one that moves no key and changes
    /// nothing a filter reads: a path ending anywhere but at a key, or passing a `Parent`, which
    /// converts to a key no object holds, and a value setter on any key but `__typename`.
    fn read(rewrite: &FetchDataRewrite) -> Option<Self> {
        let path = match rewrite {
            FetchDataRewrite::KeyRenamer(renamer) => &renamer.path,
            FetchDataRewrite::ValueSetter(setter) => &setter.path,
        };
        if !matches!(path.last(), Some(FetchDataPathElement::Key(..))) {
            return None;
        }
        let mut steps: Steps = Vec::new();
        let mut before: Vec<Element> = Vec::new();
        for element in path {
            match element {
                FetchDataPathElement::Key(key, _) => steps.push(Step {
                    before: std::mem::take(&mut before),
                    key: key.clone(),
                }),
                FetchDataPathElement::TypenameEquals(type_name) => {
                    before.push(Element::Filter(type_name.clone()))
                }
                FetchDataPathElement::AnyIndex(_) => before.push(Element::Index),
                FetchDataPathElement::Parent => return None,
            }
        }
        match rewrite {
            FetchDataRewrite::KeyRenamer(renamer) => Some(Rewrite::Rename {
                steps,
                to: renamer.rename_key_to.clone(),
            }),
            FetchDataRewrite::ValueSetter(setter) => {
                let key = &steps.last().expect("a path ending at a key").key;
                (*key == *INTROSPECTION_TYPENAME_FIELD_NAME).then(|| Rewrite::Typename {
                    value: match setter.set_value_to.as_str() {
                        Some(name) => Written::Name(name.to_owned()),
                        None => Written::Other,
                    },
                    steps,
                })
            }
        }
    }
}

/// One held location as the rewrites have left it, in one clause of configurations.
#[derive(Clone)]
struct Placed {
    /// The keys it sits under, from the copy the fetch runs on.
    keys: Vec<Name>,
    /// What a setter wrote under it, where it sat under `__typename`: the value that replaced what
    /// it held, and what was below it.
    written: Option<Written>,
    clause: Clause,
}

/// A location the representation holds, followed through the rewrites.
struct Followed {
    copy: CopyId,
    key: Name,
    /// The copies from the one the fetch runs on down to `copy`: the object holding each key.
    holders: Vec<CopyId>,
    /// The fields the representation holds at the location, each with its guard: what a filter
    /// reads where the location sits under `__typename`.
    fields: Vec<(Name, Region)>,
    /// Where it sits, in the configurations it survives in.
    placed: Vec<Placed>,
}

/// The keys held at one copy that are sent under one path, each with the configurations it is
/// sent there in holding what the representation read, and those it is sent there in holding a
/// value a setter wrote.
type Senders = Vec<(Name, Region, Region)>;

/// The representation an entity fetch sends, as its input rewrites leave it: at each copy, which
/// of the keys the representation holds there is sent under which path of keys, and where.
///
/// The rewrites are followed in their order, each over what the ones before it left, as
/// `apply_rewrites` applies them. A key renamer reaches every object its path's keys lead to as the
/// locations now sit, wherever the filters before each key admit the object holding it, and at
/// each such object holding its last key moves that location to the new name, with what is below
/// it; the location the object held under the new name, with what is below it, no longer survives.
/// A filter admits an object holding nothing under `__typename`; one holding the field
/// `__typename` there, where its runtime type is among the filter's types; and one holding a value
/// a setter wrote, where that value is the filter's type, a subtype of it, or no string. An index
/// takes a list's elements and selects nothing at an object, a filter reaches the objects of a
/// list, and where the path ends a renamer renames at an object or at each object of a list and a
/// setter writes at an object alone, as the value under each key is declared a list or not. A
/// value setter writing `__typename` writes, by the same reading, at the objects it reaches that
/// hold `__typename`, its value replacing what the location held. A move replaces what it lands
/// on, so one surviving location at most sits under one path in a configuration.
pub(super) struct SentRepresentation {
    sent: IndexMap<(CopyId, Vec<Name>), Senders>,
}

impl SentRepresentation {
    /// Follows the input rewrites over the locations the representation holds (`held`, where
    /// `projection` has them present). A filter reading a location under `__typename` that holds
    /// another field is refused: the router's filter then reads that field's value at execution,
    /// which the plan does not state. So is a rewrite reaching a value whose field the supergraph
    /// does not declare, whose list structure decides where it acts.
    pub(super) fn new(
        supergraph: &ValidFederationSchema,
        site: &Site<'_>,
        projection: &Projection,
        held: View<'_>,
        rewrites: &[Arc<FetchDataRewrite>],
    ) -> Result<Self, ComparisonError> {
        let algebra = site.hypothesis.algebra;
        let response = held.response;
        let domain = every_object(algebra);
        let anchor = site.position.copy;
        let mut followed = Vec::new();
        let mut depths: IndexMap<CopyId, Vec<(Option<usize>, Region)>> = IndexMap::default();
        for (copy, key) in projection.present.keys() {
            let fields: Vec<(Name, Region)> = held
                .fields_at(*copy, key)
                .map(|field| (field.field.clone(), field.guard.clone()))
                .collect();
            if fields.is_empty() {
                continue;
            }
            if let Some(child) = response.child(*copy, key) {
                depths
                    .entry(child)
                    .or_default()
                    .extend(held.fields_at(*copy, key).map(|field| {
                        (
                            list_depth(supergraph, &field.subgraph_parent, &field.field),
                            field.guard.clone(),
                        )
                    }));
            }
            let chain = response.ancestors(*copy);
            let Some(start) = chain.iter().position(|ancestor| *ancestor == anchor) else {
                continue;
            };
            let holders = chain[start..].to_vec();
            let mut keys: Vec<Name> = holders[1..]
                .iter()
                .filter_map(|copy| response.parent(*copy).map(|(_, key)| key.clone()))
                .collect();
            keys.push(key.clone());
            let held = algebra.simplify(
                fields
                    .iter()
                    .flat_map(|(_, guard)| guard.iter().cloned())
                    .collect(),
                &domain,
            );
            let placed = held
                .into_iter()
                .map(|clause| Placed {
                    keys: keys.clone(),
                    written: None,
                    clause,
                })
                .collect();
            followed.push(Followed {
                copy: *copy,
                key: key.clone(),
                holders,
                fields,
                placed,
            });
        }
        let mut follower = Follower {
            supergraph,
            site,
            depths,
            followed,
        };
        for rewrite in rewrites.iter().filter_map(|rewrite| Rewrite::read(rewrite)) {
            follower.apply(&rewrite)?;
        }
        let mut sent: IndexMap<(CopyId, Vec<Name>), Senders> = IndexMap::default();
        for followed in follower.followed {
            for placed in followed.placed {
                let at = sent.entry((followed.copy, placed.keys)).or_default();
                let index = match at.iter().position(|(key, _, _)| *key == followed.key) {
                    Some(index) => index,
                    None => {
                        at.push((followed.key.clone(), Vec::new(), Vec::new()));
                        at.len() - 1
                    }
                };
                match placed.written {
                    None => at[index].1.push(placed.clause),
                    Some(_) => at[index].2.push(placed.clause),
                }
            }
        }
        Ok(SentRepresentation { sent })
    }

    /// The keys held at `copy` that are sent under `path`, the keys from the copy the fetch runs
    /// on, each with the configurations it is sent in there holding what was read, and those it is
    /// sent in holding a value a setter wrote.
    pub(super) fn at(&self, copy: CopyId, path: &[Name]) -> &[(Name, Region, Region)] {
        self.sent
            .get(&(copy, path.to_vec()))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

/// The locations a representation holds, as the rewrites followed so far have left them.
struct Follower<'f> {
    supergraph: &'f ValidFederationSchema,
    site: &'f Site<'f>,
    /// How many lists deep the value of the key holding each copy is, by the fields the
    /// representation holds there, each with its guard: an object, or a list of objects.
    depths: IndexMap<CopyId, Vec<(Option<usize>, Region)>>,
    followed: Vec<Followed>,
}

/// Where the path selects the object holding each key, its filters admitting it, and where it does
/// not, by the level of the key and the object: read once per rewrite.
type Filtered = IndexMap<(usize, CopyId), (Region, Region)>;

impl Follower<'_> {
    fn apply(&mut self, rewrite: &Rewrite) -> Result<(), ComparisonError> {
        let algebra = self.site.hypothesis.algebra;
        let domain = every_object(algebra);
        let (steps, to, value, act) = match rewrite {
            Rewrite::Rename { steps, to } => (steps, Some(to), None, Act::Rename),
            Rewrite::Typename { steps, value } => (steps, None, Some(value), Act::Set),
        };
        let depth = steps.len();
        // A key renamed to itself is removed and inserted again: nothing moves.
        if to.is_some_and(|to| *to == steps[depth - 1].key) {
            return Ok(());
        }
        let reaches = |keys: &[Name]| {
            keys.len() >= depth && steps.iter().zip(keys).all(|(step, key)| step.key == *key)
        };
        let mut filtered = Filtered::default();
        // What each placement the rewrite reaches does, read from the representation as the
        // rewrites before it left it: where the filters admit it and where they refuse it.
        let mut admitted: Vec<Vec<Option<(Region, Region)>>> = Vec::new();
        // Where each object holds the key a renamer moves: what it lands on goes there.
        let mut moving: IndexMap<CopyId, Region> = IndexMap::default();
        for followed in &self.followed {
            let mut row = Vec::with_capacity(followed.placed.len());
            for placed in &followed.placed {
                algebra.work.agreement_states.add(1);
                let applies = match value {
                    // A setter writes where the object holds `__typename` itself.
                    Some(_) => placed.keys.len() == depth && reaches(&placed.keys),
                    None => reaches(&placed.keys),
                };
                if !applies {
                    row.push(None);
                    continue;
                }
                let split =
                    self.admission(&mut filtered, &followed.holders, steps, act, &placed.clause)?;
                if to.is_some() && placed.keys.len() == depth {
                    moving
                        .entry(followed.holders[depth - 1])
                        .or_default()
                        .extend(split.0.iter().cloned());
                }
                row.push(Some(split));
            }
            admitted.push(row);
        }
        for (followed, row) in self.followed.iter_mut().zip(admitted) {
            let mut placed = Vec::with_capacity(followed.placed.len());
            for (at, split) in std::mem::take(&mut followed.placed).into_iter().zip(row) {
                match (split, to) {
                    (Some((admits, refuses)), Some(to)) => {
                        for clause in admits {
                            let mut keys = at.keys.clone();
                            keys[depth - 1] = to.clone();
                            placed.push(Placed {
                                keys,
                                written: at.written.clone(),
                                clause,
                            });
                        }
                        placed.extend(refuses.into_iter().map(|clause| Placed {
                            clause,
                            ..at.clone()
                        }));
                    }
                    (Some((admits, refuses)), None) => {
                        placed.extend(admits.into_iter().map(|clause| Placed {
                            written: value.cloned(),
                            clause,
                            ..at.clone()
                        }));
                        placed.extend(refuses.into_iter().map(|clause| Placed {
                            clause,
                            ..at.clone()
                        }));
                    }
                    (None, Some(to)) => {
                        // A key moved onto this one's, at the object holding it, replaces it and
                        // what is below it.
                        let lands = at.keys.len() >= depth
                            && at.keys[depth - 1] == *to
                            && steps[..depth - 1]
                                .iter()
                                .zip(&at.keys)
                                .all(|(step, key)| step.key == *key);
                        let replaced = lands
                            .then(|| moving.get(&followed.holders[depth - 1]))
                            .flatten();
                        match replaced {
                            Some(replaced) => placed.extend(
                                algebra
                                    .subtract_all(vec![at.clause.clone()], replaced, &domain)
                                    .into_iter()
                                    .map(|clause| Placed {
                                        clause,
                                        ..at.clone()
                                    }),
                            ),
                            None => placed.push(at),
                        }
                    }
                    (None, None) => placed.push(at),
                }
            }
            followed.placed = merge(algebra, placed);
        }
        Ok(())
    }

    /// Where the path of `steps` selects each holder it reaches and every filter admits it, so that
    /// the rewrite acts there, and where it does not, within `clause`.
    fn admission(
        &self,
        filtered: &mut Filtered,
        holders: &[CopyId],
        steps: &Steps,
        act: Act,
        clause: &Clause,
    ) -> Result<(Region, Region), ComparisonError> {
        let algebra = self.site.hypothesis.algebra;
        let domain = every_object(algebra);
        let mut admits: Region = vec![clause.clone()];
        let mut refuses: Region = Vec::new();
        for (level, step) in steps.iter().enumerate() {
            let last = (level + 1 == steps.len()).then_some(act);
            let structural = step
                .before
                .iter()
                .any(|element| matches!(element, Element::Index))
                || last.is_some();
            let filters = step.filters();
            if filters.is_empty() && !structural {
                continue;
            }
            let holder = holders[level];
            if !filtered.contains_key(&(level, holder)) {
                let (selected, unselected) = match structural {
                    true => self.structure(holder, &step.before, last)?,
                    false => (everything(), Vec::new()),
                };
                let (admitted, refused) = match filters.is_empty() {
                    true => (everything(), Vec::new()),
                    false => self.filter(holder, &filters)?,
                };
                let read = (
                    algebra.simplify(algebra.product(&selected, &admitted), &domain),
                    algebra.simplify(
                        unselected
                            .into_iter()
                            .chain(algebra.product(&selected, &refused))
                            .collect(),
                        &domain,
                    ),
                );
                filtered.insert((level, holder), read);
            }
            let (admitted, refused) = &filtered[&(level, holder)];
            refuses.extend(algebra.product(&admits, refused));
            admits = algebra.product(&admits, admitted);
        }
        Ok((
            algebra.simplify(admits, &domain),
            algebra.simplify(refuses, &domain),
        ))
    }

    /// Where the path's elements `before` a key, read at the value holding the object `holder`,
    /// select it, so that a later key or, where `last` is the rewrite's act, the rewrite itself
    /// reaches it, and where they select nothing: by how many lists deep that value is declared.
    /// The representation's own object is no list.
    fn structure(
        &self,
        holder: CopyId,
        before: &[Element],
        last: Option<Act>,
    ) -> Result<(Region, Region), ComparisonError> {
        let response = self.site.hypothesis.view.response;
        let depths = match holder == self.site.position.copy {
            true => vec![(Some(0), everything())],
            false => self.depths.get(&holder).cloned().unwrap_or_default(),
        };
        let algebra = self.site.hypothesis.algebra;
        let mut selected = Vec::new();
        for (depth, region) in depths {
            let Some(depth) = depth else {
                return Err(ComparisonError::new(format!(
                    "fetch to {}: an input rewrite reaches {}, where the supergraph declares no \
                     field whose list structure the checker can read",
                    self.site.fetch.subgraph_name,
                    response.describe(holder),
                )));
            };
            if selects(before, depth, last) {
                selected.extend(region);
            }
        }
        let unselected = algebra.subtract_all(everything(), &selected, &every_object(algebra));
        Ok((selected, unselected))
    }

    /// Where `filters` admit the object `holder` and where they refuse it, by what the
    /// representation holds under its `__typename` now.
    fn filter(
        &self,
        holder: CopyId,
        filters: &[Name],
    ) -> Result<(Region, Region), ComparisonError> {
        let hypothesis = self.site.hypothesis;
        let algebra = hypothesis.algebra;
        let response = hypothesis.view.response;
        let domain = every_object(algebra);
        let types = algebra.objects(&admitted_types(self.supergraph, filters).unwrap_or_default());
        let inside = response.type_literal(algebra, holder, types);
        let outside = response.type_literal(
            algebra,
            holder,
            algebra.minus(algebra.every_object(), types),
        );
        let names = |name: &str| {
            filters.iter().all(|filter| {
                filter.as_str() == name || self.supergraph.schema().is_subtype(filter, name)
            })
        };
        let (mut admits, mut refuses, mut typed) = (Vec::new(), Vec::new(), Vec::new());
        for followed in self
            .followed
            .iter()
            .filter(|followed| followed.copy == holder)
        {
            for placed in &followed.placed {
                if placed.keys.last() != Some(&INTROSPECTION_TYPENAME_FIELD_NAME) {
                    continue;
                }
                if let Some(written) = &placed.written {
                    typed.push(placed.clause.clone());
                    match written {
                        Written::Name(name) if !names(name) => refuses.push(placed.clause.clone()),
                        _ => admits.push(placed.clause.clone()),
                    }
                    continue;
                }
                for (field, guard) in &followed.fields {
                    for clause in algebra.product(std::slice::from_ref(&placed.clause), guard) {
                        typed.push(clause.clone());
                        if *field != *INTROSPECTION_TYPENAME_FIELD_NAME {
                            if let Some(example) = hypothesis.uncovered(holder, &clause, &[]) {
                                return Err(ComparisonError::new(format!(
                                    "fetch to {}: an input rewrite's `TypenameEquals({})` tests \
                                     {}, where the representation holds `{field}` under \
                                     `__typename`, which it cannot read as a type",
                                    self.site.fetch.subgraph_name,
                                    filters
                                        .iter()
                                        .map(Name::as_str)
                                        .collect::<Vec<_>>()
                                        .join(", "),
                                    describe_at(hypothesis, holder, example),
                                )));
                            }
                            continue;
                        }
                        admits.extend(inside.as_ref().and_then(|t| algebra.conjoin(&clause, t)));
                        refuses.extend(outside.as_ref().and_then(|t| algebra.conjoin(&clause, t)));
                    }
                }
            }
        }
        // An object holding nothing under `__typename` is admitted.
        admits.extend(algebra.subtract_all(everything(), &typed, &domain));
        Ok((
            algebra.simplify(admits, &domain),
            algebra.simplify(refuses, &domain),
        ))
    }
}

/// One location's placements with those sitting alike joined, as few clauses as hold the same
/// configurations.
fn merge(algebra: &Algebra, placed: Vec<Placed>) -> Vec<Placed> {
    let domain = every_object(algebra);
    let mut alike: Vec<(Vec<Name>, Option<Written>, Region)> = Vec::new();
    for at in placed {
        match alike
            .iter_mut()
            .find(|(keys, written, _)| *keys == at.keys && *written == at.written)
        {
            Some((_, _, region)) => region.push(at.clause),
            None => alike.push((at.keys, at.written, vec![at.clause])),
        }
    }
    alike
        .into_iter()
        .flat_map(|(keys, written, region)| {
            algebra
                .simplify(region, &domain)
                .into_iter()
                .map(move |clause| Placed {
                    keys: keys.clone(),
                    written: written.clone(),
                    clause,
                })
        })
        .collect()
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
    /// and the `@requires` half at the entity type, read where the input rewrites send it, and
    /// some resolvable key covers it in each configuration.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn check_representations(
        &self,
        fetch: &FetchNode,
        subgraph: &Subgraph<'_>,
        fragments: &[(&Name, &Node<InlineFragment>)],
        entity_types: &[(Name, Vec<Name>)],
        projection: &Projection,
        held: View<'_>,
        sent: &SentRepresentation,
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
                    sent,
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
        sent: &SentRepresentation,
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
            sent: Some(sent),
        };
        let at = response.describe(copy);
        // The demand of each key that applies at the runtime type, as the subgraph reads it.
        let mut demands: Vec<(&KeyDirective, Vec<Selection>)> = Vec::new();
        let mut first: Option<String> = None;
        for key in keys {
            algebra.work.key_candidates.add(1);
            // A key that does not apply at the runtime type is no key of it.
            let Ok(mut demand) = key_half(self.supergraph_schema, object, &key.fields) else {
                continue;
            };
            demand.extend(requires.iter().cloned());
            let required = wrap_non_empty(
                |selections| inline_fragment(object.clone(), selections),
                demand,
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
