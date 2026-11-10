//! What an entity fetch sends, and what it must find to send it.
//!
//! Port of the representation, `keysInclude`, overwritten-key and input-rewrite sections of
//! `Apollo/Definitions/QueryPlanSoundness.lean`.
//!
//! What the fetch can read and what the subgraph demands are the authorities, linked by the input
//! rewrites; a `requires` entry only filters them ([`selected_by_one_pass`], [`invert_through`]).
//! An object's representation is every entry whose type condition admits it, deep-merged, so the
//! entries are checked together, one entity case at a time.
//!
//! - What an entry must already have fetched of a demand is the demand narrowed by the entry's
//!   conditions and taken back through the input rewrites as the entry reads it
//!   ([`required_selections`]): an alias is taken back only where the entry reads it, so per
//!   runtime type, as the router builds a representation through the fragments that apply to it.
//! - What an entry sends of a demand is the demand narrowed by its conditions and selected by what
//!   it sends ([`sent_selections`]). The router renames what the entry read instead, which comes to
//!   the same where the rewrites are well formed, idempotent on the demand, and agree with the
//!   entry (the model's `RequiredSelectionsRenameToSent`).
//!
//! Below a field an entry copies whole, which agreement cannot reach, the input rewrites are held
//! to three structural checks: [`input_rewrites_well_formed`], [`input_rewrites_separated`] and
//! [`input_rewrites_idempotent`]. An input value setter may set only a representation's
//! `__typename`, and [`sent_typename`] follows the setters to the name each object is sent as.

use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::schema::ExtendedType;

use super::super::query_compare::conditions::BooleanCondition;
use super::super::query_compare::conditions::BooleanLiteral;
use super::super::query_compare::conditions::boolean_condition_covered_by;
use super::super::query_compare::conditions::canonical_boolean_condition;
use super::super::query_compare::conditions::literals_for_directives;
use super::context::display_path;
use super::requires::same_keys;
use super::returned::ReturnedTypes;
use super::selections::admitted_types;
use super::selections::apply_key_renamers;
use super::selections::condition_directives;
use super::selections::inline_fragment_under;
use super::selections::possible_types;
use super::selections::selection_set;
use super::selections::type_filter_meets;
use super::selections::type_includes_object;
use super::selections::wrap_non_empty;
use crate::operation::TYPENAME_FIELD;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
use crate::query_plan::FetchNode;
use crate::query_plan::requires_selection;
use crate::schema::ValidFederationSchema;

//==================================================================================================
// An entry's conditions
//==================================================================================================

/// The fragments under which a `requires` selection writes a top-level key, one chain per way,
/// outermost first. A fragment holding a field directly writes wherever it is entered, so a chain
/// stops there; one holding only fragments continues into each.
pub(super) fn key_guards(
    selection: &requires_selection::Selection,
) -> Vec<Vec<&requires_selection::InlineFragment>> {
    match selection {
        requires_selection::Selection::Field(_) => vec![Vec::new()],
        requires_selection::Selection::InlineFragment(fragment) => {
            set_key_guards(&fragment.selections)
                .into_iter()
                .map(|mut chain| {
                    chain.insert(0, fragment);
                    chain
                })
                .collect()
        }
    }
}

fn set_key_guards(
    selections: &[requires_selection::Selection],
) -> Vec<Vec<&requires_selection::InlineFragment>> {
    let mut out = Vec::new();
    for selection in selections {
        let here = key_guards(selection);
        if here.iter().any(Vec::is_empty) {
            return vec![Vec::new()];
        }
        out.extend(here);
    }
    out
}

/// `selections` wherever a `requires` entry writes a key: under each of its key guards. An entry
/// writing none sends no representation, which `Variables::new` drops, so nothing is owed or
/// fetched there.
pub(super) fn narrowed_by_entry_condition(
    entry: &requires_selection::Selection,
    parent_type: &Name,
    selections: Vec<Selection>,
) -> Vec<Selection> {
    let mut out = Vec::new();
    for chain in key_guards(entry) {
        let mut wrapped = selections.clone();
        for fragment in chain.iter().rev() {
            wrapped = wrap_non_empty(
                |selections| {
                    inline_fragment_under(
                        fragment.type_condition.as_ref(),
                        parent_type,
                        &condition_directives(&fragment.conditions),
                        selections,
                    )
                },
                wrapped,
            );
        }
        out.extend(wrapped);
    }
    out
}

/// Whether an entry writes its keys under its own outermost fragment alone: one chain, that
/// fragment, carrying no conditions. What it credits is then that fragment around the selections.
pub(super) fn writes_under_its_own_type(entry: &requires_selection::Selection) -> bool {
    match (entry, key_guards(entry).as_slice()) {
        (requires_selection::Selection::InlineFragment(fragment), [chain]) => {
            matches!(chain.as_slice(), [only] if std::ptr::eq(*only, fragment))
                && fragment.conditions.is_empty()
        }
        _ => false,
    }
}

/// `selections` wherever one of `entries` writes a key ([`narrowed_by_entry_condition`]).
pub(super) fn narrowed_by_entry_conditions(
    entries: &[&requires_selection::Selection],
    parent_type: &Name,
    selections: &[Selection],
) -> Vec<Selection> {
    entries
        .iter()
        .flat_map(|entry| narrowed_by_entry_condition(entry, parent_type, selections.to_vec()))
        .collect()
}

/// Every complete assignment of the variables `entries`' conditions read, as the literals that
/// fix it. What the router sends, and so the key the subgraph resolves it by, changes only with
/// these.
pub(super) fn entry_configurations(
    entries: &[&requires_selection::Selection],
) -> Vec<BooleanCondition> {
    fn collect(selection: &requires_selection::Selection, out: &mut Vec<Name>) {
        match selection {
            requires_selection::Selection::Field(field) => {
                field.selections.iter().for_each(|s| collect(s, out))
            }
            requires_selection::Selection::InlineFragment(fragment) => {
                for condition in &fragment.conditions {
                    if let requires_selection::ConditionValue::Variable(name) = &condition.if_
                        && !out.contains(name)
                    {
                        out.push(name.clone());
                    }
                }
                fragment.selections.iter().for_each(|s| collect(s, out))
            }
        }
    }
    let mut variables = Vec::new();
    entries
        .iter()
        .for_each(|entry| collect(entry, &mut variables));
    let mut configurations: Vec<BooleanCondition> = vec![Vec::new()];
    for variable in variables.iter().rev() {
        configurations = configurations
            .into_iter()
            .flat_map(|configuration| {
                [true, false].map(|positive| {
                    let literal = if positive {
                        BooleanLiteral::Positive(variable.clone())
                    } else {
                        BooleanLiteral::Negative(variable.clone())
                    };
                    let mut extended = vec![literal];
                    extended.extend(configuration.iter().cloned());
                    extended
                })
            })
            .collect();
    }
    configurations
}

//==================================================================================================
// Selecting by an entry
//==================================================================================================

/// `source` filtered by a run of an entry's fields, in one pass: each source field one of `fields`
/// selects, below it `source` whole if one selects it bare and filtered by what the others select,
/// and each source fragment kept once around them. The model's `SelectionSet.keptBy`.
fn kept_by(fields: &[&apollo_compiler::executable::Field], source: &[Selection]) -> Vec<Selection> {
    let mut out = Vec::new();
    for selection in source {
        match selection {
            Selection::Field(field) => {
                let key = field.response_name();
                let at_key: Vec<_> = fields
                    .iter()
                    .filter(|entry| entry.response_name() == key)
                    .collect();
                if at_key.is_empty() {
                    continue;
                }
                let below = &field.selection_set.selections;
                let mut kept = if at_key
                    .iter()
                    .any(|entry| entry.selection_set.selections.is_empty())
                {
                    below.clone()
                } else {
                    Vec::new()
                };
                let entry_below: Vec<Selection> = at_key
                    .iter()
                    .flat_map(|entry| entry.selection_set.selections.iter().cloned())
                    .collect();
                kept.extend(selected_by_one_pass(below, &entry_below));
                let mut copy = (**field).clone();
                copy.selection_set.selections = kept;
                out.push(Selection::Field(Node::new(copy)));
            }
            Selection::InlineFragment(fragment) => out.extend(wrap_non_empty(
                |selections| {
                    let mut copy = (**fragment).clone();
                    copy.selection_set.selections = selections;
                    Selection::InlineFragment(Node::new(copy))
                },
                kept_by(fields, &fragment.selection_set.selections),
            )),
            Selection::FragmentSpread(_) => {}
        }
    }
    out
}

/// `source` filtered by an entry's reading: what `source` selects under each key the entry
/// selects, as `source` writes it, inside the entry's fragments. Below a bare entry field, which
/// copies the whole value, `source` is kept whole. Nothing is added.
///
/// The model's `SelectionSet.selectedByOnePass`: the source is filtered once per run of the
/// entry's fields ([`kept_by`]), where the specification's `selectedBy` filters it once per field
/// and copies its fragments around every key. The two compare alike by inclusion wherever the
/// checker mounts them (`selectedByOnePass_correct`); one name's occurrences keep their source
/// order, and only different names may interleave differently.
pub(super) fn selected_by_one_pass(source: &[Selection], entry: &[Selection]) -> Vec<Selection> {
    let mut out = Vec::new();
    let mut rest = entry;
    while let Some((first, tail)) = rest.split_first() {
        match first {
            Selection::Field(_) => {
                let run = rest
                    .iter()
                    .take_while(|selection| matches!(selection, Selection::Field(_)))
                    .count();
                let fields: Vec<&apollo_compiler::executable::Field> = rest[..run]
                    .iter()
                    .filter_map(|selection| match selection {
                        Selection::Field(field) => Some(&**field),
                        _ => None,
                    })
                    .collect();
                out.extend(kept_by(&fields, source));
                rest = &rest[run..];
            }
            Selection::InlineFragment(fragment) => {
                out.extend(wrap_non_empty(
                    |selections| {
                        Selection::InlineFragment(Node::new(
                            apollo_compiler::executable::InlineFragment {
                                type_condition: fragment.type_condition.clone(),
                                directives: fragment.directives.clone(),
                                selection_set: selection_set(
                                    fragment.selection_set.ty.clone(),
                                    selections,
                                ),
                            },
                        ))
                    },
                    selected_by_one_pass(source, &fragment.selection_set.selections),
                ));
                rest = tail;
            }
            Selection::FragmentSpread(_) => rest = tail,
        }
    }
    out
}

/// What an entry must already have fetched to send `demand`: the demand where the entry writes a
/// key, taken back through the input rewrites as the entry reads it (`as_read`, [`invert_through`]).
pub(super) fn required_selections(
    fetch: &FetchNode,
    entry: &requires_selection::Selection,
    as_read: &Selection,
    parent_type: &Name,
    demand: &[Selection],
) -> Vec<Selection> {
    invert_through(
        &key_renamings(fetch),
        &narrowed_by_entry_condition(entry, parent_type, demand.to_vec()),
        std::slice::from_ref(as_read),
    )
}

/// What an entry sends of `demand`: the demand where the entry writes a key, selected by what the
/// entry sends (`as_sent`).
pub(super) fn sent_selections(
    entry: &requires_selection::Selection,
    as_sent: &Selection,
    parent_type: &Name,
    demand: &[Selection],
) -> Vec<Selection> {
    selected_by_one_pass(
        &narrowed_by_entry_condition(entry, parent_type, demand.to_vec()),
        std::slice::from_ref(as_sent),
    )
}

//==================================================================================================
// Where the input rewrites rename
//==================================================================================================

/// A path of response keys into a representation: a fetch data path without its type tests and
/// list indices.
type KeyPath = Vec<Name>;

/// Where a key renamer renames: the key path above the key it renames, that key, and the key it
/// renames it to.
#[derive(Clone, PartialEq)]
struct KeyRenaming {
    above: KeyPath,
    old: Name,
    new: Name,
}

impl KeyRenaming {
    /// The key path it renames from.
    fn old_path(&self) -> KeyPath {
        let mut path = self.above.clone();
        path.push(self.old.clone());
        path
    }

    /// The key path it renames to.
    fn new_path(&self) -> KeyPath {
        let mut path = self.above.clone();
        path.push(self.new.clone());
        path
    }
}

/// Where a key renamer at `path` renames to `new_key`, as `rename_key_at` renames: its keys
/// followed, `AnyIndex` elements passed over. A path ending at no key or climbing with `Parent`
/// renames nothing; nor, here, does one testing a `__typename`, which well formed input rewrites
/// never do.
fn key_renaming(path: &[FetchDataPathElement], new_key: &Name) -> Option<KeyRenaming> {
    let (FetchDataPathElement::Key(old, _), leading) = path.split_last()? else {
        return None;
    };
    let mut above = Vec::new();
    for element in leading {
        match element {
            FetchDataPathElement::Key(name, _) => above.push(name.clone()),
            FetchDataPathElement::AnyIndex(_) => {}
            FetchDataPathElement::TypenameEquals(_) | FetchDataPathElement::Parent => return None,
        }
    }
    Some(KeyRenaming {
        above,
        old: old.clone(),
        new: new_key.clone(),
    })
}

/// Where the fetch's input rewrites rename, in the order `apply_rewrites` applies them; a value
/// setter renames nothing.
fn key_renamings(fetch: &FetchNode) -> Vec<KeyRenaming> {
    fetch
        .input_rewrites
        .iter()
        .filter_map(|rewrite| match rewrite.as_ref() {
            FetchDataRewrite::KeyRenamer(renamer) => {
                key_renaming(&renamer.path, &renamer.rename_key_to)
            }
            FetchDataRewrite::ValueSetter(_) => None,
        })
        .collect()
}

/// The key a value read under `key` is sent under: each renaming of one key applied in order, as
/// `apply_rewrites` moves the value.
fn sent_key(renamings: &[KeyRenaming], key: &Name) -> Name {
    renamings
        .iter()
        .filter(|step| step.above.is_empty())
        .fold(key.clone(), |key, step| {
            if step.old == key {
                step.new.clone()
            } else {
                key
            }
        })
}

/// The key a value sent under `key` was read under: each renaming of one key taken back, the last
/// first, so that of a key read under its own name and an alias renamed onto it, the alias is
/// chosen.
fn read_key(renamings: &[KeyRenaming], key: &Name) -> Name {
    renamings
        .iter()
        .rev()
        .filter(|step| step.above.is_empty())
        .fold(key.clone(), |key, step| {
            if step.new == key {
                step.old.clone()
            } else {
                key
            }
        })
}

/// The renamings reaching below the value read under `key`, their paths taken from below it, each
/// as the value's key stood when it ran ([`sent_key`]).
fn renamings_below(renamings: &[KeyRenaming], key: &Name) -> Vec<KeyRenaming> {
    let mut key = key.clone();
    let mut out = Vec::new();
    for step in renamings {
        match step.above.split_first() {
            None => {
                if step.old == key {
                    key = step.new.clone();
                }
            }
            Some((name, above)) => {
                if *name == key {
                    out.push(KeyRenaming {
                        above: above.to_vec(),
                        old: step.old.clone(),
                        new: step.new.clone(),
                    });
                }
            }
        }
    }
    out
}

/// `field` under response key `key`, `selections` below it.
fn field_under_key(
    field: &apollo_compiler::executable::Field,
    key: &Name,
    selections: Vec<Selection>,
) -> Selection {
    let mut copy = field.clone();
    copy.alias = (*key != copy.name).then(|| key.clone());
    copy.selection_set.selections = selections;
    Selection::Field(Node::new(copy))
}

/// A selection set taken back through the renamings by spelling: each field put back under the key
/// it was read under ([`read_key`]), and what is below it likewise. This is all that can be said
/// below a field an entry copies whole, which says nothing of what it holds.
fn invert_selections(renamings: &[KeyRenaming], source: &[Selection]) -> Vec<Selection> {
    source
        .iter()
        .filter_map(|selection| match selection {
            Selection::Field(field) => {
                let key = read_key(renamings, field.response_name());
                let below = invert_selections(
                    &renamings_below(renamings, &key),
                    &field.selection_set.selections,
                );
                Some(field_under_key(field, &key, below))
            }
            Selection::InlineFragment(fragment) => {
                let mut copy = (**fragment).clone();
                copy.selection_set.selections =
                    invert_selections(renamings, &fragment.selection_set.selections);
                Some(Selection::InlineFragment(Node::new(copy)))
            }
            Selection::FragmentSpread(_) => None,
        })
        .collect()
}

/// One field of an entry as [`invert_through`] reads it: the key it is read under, the key the
/// renamings send it under ([`sent_key`]), and the renamings reaching below it.
struct ReadField<'a> {
    field: &'a apollo_compiler::executable::Field,
    sent: Name,
    below: Vec<KeyRenaming>,
}

/// `source` taken back for a run of an entry's fields, in one pass: each source field one of
/// `fields` is sent under, put under the key that field reads, what is below taken back likewise
/// ([`invert_selections`] below a field the entry copies whole), and each source fragment kept once
/// around them.
fn taken_back_by(fields: &[ReadField<'_>], source: &[Selection]) -> Vec<Selection> {
    let mut out = Vec::new();
    for selection in source {
        match selection {
            Selection::Field(source_field) => {
                for read in fields
                    .iter()
                    .filter(|read| read.sent == *source_field.response_name())
                {
                    let source_below = &source_field.selection_set.selections;
                    let entry_below = &read.field.selection_set.selections;
                    let below = if entry_below.is_empty() {
                        invert_selections(&read.below, source_below)
                    } else {
                        invert_through(&read.below, source_below, entry_below)
                    };
                    out.push(field_under_key(
                        source_field,
                        read.field.response_name(),
                        below,
                    ));
                }
            }
            Selection::InlineFragment(fragment) => out.extend(wrap_non_empty(
                |selections| {
                    let mut copy = (**fragment).clone();
                    copy.selection_set.selections = selections;
                    Selection::InlineFragment(Node::new(copy))
                },
                taken_back_by(fields, &fragment.selection_set.selections),
            )),
            Selection::FragmentSpread(_) => {}
        }
    }
    out
}

/// `source` taken back through the renamings as an entry reads it: for each key the entry reads,
/// what `source` selects under the key it is sent under ([`sent_key`]), put under the key read,
/// inside the entry's fragments. Which keys an object holds follows its type, as the router builds
/// a representation through the fragments that apply to it, so a key is taken back only where the
/// entry reads it. Below a field the entry copies whole, `source` is taken back by spelling
/// ([`invert_selections`]).
///
/// The model's `KeyRenamings.invertThrough`, run once per run of the entry's fields
/// ([`taken_back_by`]) rather than once per field, as [`selected_by_one_pass`] runs `selectedBy`:
/// one key's occurrences keep their source order, and only different keys may interleave
/// differently.
fn invert_through(
    renamings: &[KeyRenaming],
    source: &[Selection],
    entry: &[Selection],
) -> Vec<Selection> {
    let mut out = Vec::new();
    let mut rest = entry;
    while let Some((first, tail)) = rest.split_first() {
        match first {
            Selection::Field(_) => {
                let run = rest
                    .iter()
                    .take_while(|selection| matches!(selection, Selection::Field(_)))
                    .count();
                let fields: Vec<ReadField<'_>> = rest[..run]
                    .iter()
                    .filter_map(|selection| match selection {
                        Selection::Field(field) => Some(ReadField {
                            field,
                            sent: sent_key(renamings, field.response_name()),
                            below: renamings_below(renamings, field.response_name()),
                        }),
                        _ => None,
                    })
                    .collect();
                out.extend(taken_back_by(&fields, source));
                rest = &rest[run..];
            }
            Selection::InlineFragment(fragment) => {
                out.extend(wrap_non_empty(
                    |selections| {
                        Selection::InlineFragment(Node::new(
                            apollo_compiler::executable::InlineFragment {
                                type_condition: fragment.type_condition.clone(),
                                directives: fragment.directives.clone(),
                                selection_set: selection_set(
                                    fragment.selection_set.ty.clone(),
                                    selections,
                                ),
                            },
                        ))
                    },
                    invert_through(renamings, source, &fragment.selection_set.selections),
                ));
                rest = tail;
            }
            Selection::FragmentSpread(_) => rest = tail,
        }
    }
    out
}

//==================================================================================================
// The input rewrites
//==================================================================================================

/// Whether a key renamer tests the type of an object it reaches, with a `TypenameEquals` or a
/// type condition on a key or index, renaming for some types and not others. A value setter may:
/// it changes a value, never a key.
fn filters_by_type(rewrite: &FetchDataRewrite) -> bool {
    match rewrite {
        FetchDataRewrite::KeyRenamer(renamer) => renamer.path.iter().any(|element| match element {
            FetchDataPathElement::TypenameEquals(_) => true,
            FetchDataPathElement::Key(_, condition) | FetchDataPathElement::AnyIndex(condition) => {
                condition.is_some()
            }
            FetchDataPathElement::Parent => false,
        }),
        FetchDataRewrite::ValueSetter(_) => false,
    }
}

/// Whether a value setter's path sets the `__typename` of a representation sent as `typename`: it
/// ends there ([`ends_at_typename`]), and each `TypenameEquals` on the way matches `typename`, as
/// `is_object_of_type` matches a type or one of its subtypes.
fn sets_typename_of(
    schema: &ValidFederationSchema,
    typename: &Name,
    path: &[FetchDataPathElement],
) -> bool {
    ends_at_typename(path)
        && path.iter().all(|element| match element {
            FetchDataPathElement::TypenameEquals(type_name) => {
                type_name == typename || type_includes_object(schema, type_name, typename)
            }
            _ => true,
        })
}

/// The `__typename` an object of type `object_type` is sent as: its own, set in turn by each input
/// value setter ending at it ([`sets_typename_of`]), as `apply_rewrites` applies the rewrites in
/// order. `None` once one sets it to anything but a type name. The planner writes one to send an
/// interface's name (`compute_input_rewrites_on_key_fetch`).
pub(super) fn sent_typename(
    schema: &ValidFederationSchema,
    rewrites: &[Arc<FetchDataRewrite>],
    object_type: &Name,
) -> Option<Name> {
    let mut typename = object_type.clone();
    for rewrite in rewrites {
        let FetchDataRewrite::ValueSetter(setter) = rewrite.as_ref() else {
            continue;
        };
        if !sets_typename_of(schema, &typename, &setter.path) {
            continue;
        }
        // A string that is no GraphQL name names no type, so it can never be the case's.
        typename = setter
            .set_value_to
            .as_str()
            .and_then(|value| Name::new(value).ok())?;
    }
    Some(typename)
}

fn display_key_path(path: &[Name]) -> String {
    path.iter().map(Name::as_str).collect::<Vec<_>>().join(".")
}

/// Whether a fetch data path ends at a representation's own `__typename`: every element before its
/// last a `TypenameEquals`.
fn ends_at_typename(path: &[FetchDataPathElement]) -> bool {
    match path.split_last() {
        Some((FetchDataPathElement::Key(name, _), before)) => {
            *name == TYPENAME_FIELD
                && before
                    .iter()
                    .all(|element| matches!(element, FetchDataPathElement::TypenameEquals(_)))
        }
        _ => false,
    }
}

/// The value setter that sets anything but a representation's own `__typename`
/// ([`ends_at_typename`]), such as a key field, which would send another entity's key.
fn sets_more_than_typename(rewrite: &FetchDataRewrite) -> Option<&[FetchDataPathElement]> {
    match rewrite {
        FetchDataRewrite::ValueSetter(setter) if !ends_at_typename(&setter.path) => {
            Some(&setter.path)
        }
        _ => None,
    }
}

/// The input rewrites set no value but `__typename`, rename each key path one way, whatever the
/// types and conditions they meet, and never rename what they renamed or what lies below what they
/// rename: every value setter sets only `__typename`, no key renamer filters by type, two renaming
/// from one key path rename it to one, none renames from below a key path another renames from, and
/// none renames from a key path one renames to. `Err` says which.
pub(super) fn input_rewrites_well_formed(fetch: &FetchNode) -> Result<(), String> {
    if let Some(path) = fetch
        .input_rewrites
        .iter()
        .find_map(|rewrite| sets_more_than_typename(rewrite))
    {
        return Err(format!(
            "an input value setter sets `{}`, not a representation's `__typename`",
            display_path(path)
        ));
    }
    if fetch
        .input_rewrites
        .iter()
        .any(|rewrite| filters_by_type(rewrite))
    {
        return Err("an input key renamer tests the type of what it renames".to_string());
    }
    let renamings = key_renamings(fetch);
    for one in &renamings {
        for other in &renamings {
            if other.old_path().starts_with(&one.old_path()) && one != other {
                return Err(format!(
                    "input key renamers rename `{}` and `{}` below it two ways",
                    display_key_path(&one.old_path()),
                    display_key_path(&other.old_path())
                ));
            }
            if one.old_path() == other.new_path() {
                return Err(format!(
                    "an input key renamer renames `{}`, which another renames to",
                    display_key_path(&one.old_path())
                ));
            }
        }
    }
    Ok(())
}

/// The object types the fragments around each key of a key path admit at that key's level
/// ([`admitted_types`]), `None` where none narrows it.
type KeyPathTypeCondition = Vec<Option<Vec<Name>>>;

/// The type condition of each place an entry selection reads the key path `path`, `guards` the
/// fragments in force at its first key. Where `allow_copy`, a field copied whole on the way reads
/// every key path below it, at any type.
fn entry_key_path_type_conditions(
    schema: &ValidFederationSchema,
    allow_copy: bool,
    guards: &[Name],
    selection: &requires_selection::Selection,
    path: &[Name],
) -> Vec<KeyPathTypeCondition> {
    match selection {
        requires_selection::Selection::Field(field) => {
            let Some((key, rest)) = path.split_first() else {
                return Vec::new();
            };
            if field.alias.as_ref().unwrap_or(&field.name) != key {
                return Vec::new();
            }
            let here = admitted_types(schema, guards);
            if rest.is_empty() {
                vec![vec![here]]
            } else if allow_copy && field.selections.is_empty() {
                let mut condition = vec![here];
                condition.extend(rest.iter().map(|_| None));
                vec![condition]
            } else {
                field
                    .selections
                    .iter()
                    .flat_map(|selection| {
                        entry_key_path_type_conditions(schema, allow_copy, &[], selection, rest)
                    })
                    .map(|below| {
                        let mut condition = vec![here.clone()];
                        condition.extend(below);
                        condition
                    })
                    .collect()
            }
        }
        requires_selection::Selection::InlineFragment(fragment) => {
            let mut inner_guards = guards.to_vec();
            inner_guards.extend(fragment.type_condition.iter().cloned());
            fragment
                .selections
                .iter()
                .flat_map(|selection| {
                    entry_key_path_type_conditions(
                        schema,
                        allow_copy,
                        &inner_guards,
                        selection,
                        path,
                    )
                })
                .collect()
        }
    }
}

/// The type condition of each place the fetch's entries read the key path `path`
/// ([`entry_key_path_type_conditions`]): the representations that may hold it.
fn key_path_type_conditions(
    schema: &ValidFederationSchema,
    fetch: &FetchNode,
    allow_copy: bool,
    path: &[Name],
) -> Vec<KeyPathTypeCondition> {
    fetch
        .requires
        .iter()
        .flat_map(|entry| match entry {
            requires_selection::Selection::InlineFragment(fragment)
                if fragment.type_condition.is_some() =>
            {
                entry_key_path_type_conditions(schema, allow_copy, &[], entry, path)
            }
            _ => Vec::new(),
        })
        .collect()
}

/// Whether two key paths through the same objects can be held by one object: whether their type
/// conditions admit a common object type at every level, `None` admitting every one.
fn key_path_type_conditions_meet(one: &KeyPathTypeCondition, other: &KeyPathTypeCondition) -> bool {
    std::iter::zip(one, other).all(|(here, other_here)| match (here, other_here) {
        (Some(types), Some(other_types)) => types.iter().any(|ty| other_types.contains(ty)),
        _ => true,
    })
}

/// The key paths the input rewrites send under the key path `key`, each with whether it counts as
/// read where an entry copies whole a field on the way to it: `key` itself, read under its own
/// name, does not; each key path renamed onto it does, as the copy gives no evidence of what it
/// holds.
fn sent_under(fetch: &FetchNode, key: &[Name]) -> Vec<(KeyPath, bool)> {
    let mut out = vec![(key.to_vec(), false)];
    out.extend(
        key_renamings(fetch)
            .into_iter()
            .filter(|renamed| renamed.new_path() == key)
            .map(|renamed| (renamed.old_path(), true)),
    );
    out
}

/// No representation holds two key paths the input rewrites send under one key, as fields under
/// one response key must merge: no type conditions of the places they are read meet
/// ([`key_path_type_conditions_meet`]). `Err` names the key.
pub(super) fn input_rewrites_separated(
    schema: &ValidFederationSchema,
    fetch: &FetchNode,
) -> Result<(), String> {
    for renamed in key_renamings(fetch) {
        let sent = sent_under(fetch, &renamed.new_path());
        for one in &sent {
            for other in &sent {
                if one.0 == other.0 {
                    continue;
                }
                let other_conditions = key_path_type_conditions(schema, fetch, other.1, &other.0);
                let held_together = key_path_type_conditions(schema, fetch, one.1, &one.0)
                    .iter()
                    .any(|condition| {
                        other_conditions.iter().any(|other_condition| {
                            key_path_type_conditions_meet(condition, other_condition)
                        })
                    });
                if held_together {
                    return Err(format!(
                        "input key renamers send `{}` and `{}` under one key `{}` in one representation",
                        display_key_path(&one.0),
                        display_key_path(&other.0),
                        display_key_path(&renamed.new_path())
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The input rewrites are idempotent on a demand: renamed by them, the demand keeps its keys, so
/// they rename only aliases, never a key the subgraph demands.
pub(super) fn input_rewrites_idempotent(
    schema: &ValidFederationSchema,
    fetch: &FetchNode,
    demand: &[Selection],
) -> bool {
    same_keys(
        &apply_key_renamers(
            schema,
            type_filter_meets,
            &fetch.input_rewrites,
            demand.to_vec(),
        ),
        demand,
    )
}

//==================================================================================================
// Keys an entry overwrites
//==================================================================================================

/// The keys a selection writes at its own level, through its inline fragments.
fn keys_written(selection: &requires_selection::Selection, out: &mut Vec<Name>) {
    match selection {
        requires_selection::Selection::Field(field) => {
            out.push(field.alias.as_ref().unwrap_or(&field.name).clone())
        }
        requires_selection::Selection::InlineFragment(fragment) => fragment
            .selections
            .iter()
            .for_each(|selection| keys_written(selection, out)),
    }
}

/// The keys a field of a field set overwrites, an earlier selection of the same list having written
/// them, at any level: `execute_selection_set` replaces a field's value under its key, and
/// deep-merges only a fragment's.
pub(super) fn overwritten_keys(selections: &[requires_selection::Selection]) -> Vec<Name> {
    let mut out = Vec::new();
    for (index, selection) in selections.iter().enumerate() {
        match selection {
            requires_selection::Selection::Field(field) => {
                out.extend(overwritten_keys(&field.selections))
            }
            requires_selection::Selection::InlineFragment(fragment) => {
                out.extend(overwritten_keys(&fragment.selections))
            }
        }
        let mut written = Vec::new();
        keys_written(selection, &mut written);
        out.extend(
            selections[index + 1..]
                .iter()
                .filter_map(|later| match later {
                    requires_selection::Selection::Field(field) => {
                        Some(field.alias.as_ref().unwrap_or(&field.name).clone())
                    }
                    requires_selection::Selection::InlineFragment(_) => None,
                })
                .filter(|key| written.contains(key)),
        );
    }
    out
}

//==================================================================================================
// Inclusion by response key, arguments aside
//==================================================================================================

/// The object types a type name grounds to, and a field's result types, looked up once per check:
/// the key walk meets the same names at every copy of a position, and in every fetch.
pub(super) struct SchemaTypeLookup<'s> {
    schema: &'s ValidFederationSchema,
    possible_types_by_type: std::cell::RefCell<IndexMap<Name, Arc<Vec<Name>>>>,
    result_types_by_field: std::cell::RefCell<IndexMap<ObjectField, Arc<Vec<Name>>>>,
}

/// A field, by the object type it is selected at and its name.
type ObjectField = (Name, Name);

impl<'s> SchemaTypeLookup<'s> {
    pub(super) fn new(schema: &'s ValidFederationSchema) -> Self {
        SchemaTypeLookup {
            schema,
            possible_types_by_type: Default::default(),
            result_types_by_field: Default::default(),
        }
    }

    pub(super) fn possible_types(&self, type_name: &Name) -> Arc<Vec<Name>> {
        if let Some(types) = self.possible_types_by_type.borrow().get(type_name) {
            return types.clone();
        }
        let types = Arc::new(possible_types(self.schema, type_name));
        self.possible_types_by_type
            .borrow_mut()
            .insert(type_name.clone(), types.clone());
        types
    }

    /// The object types a field returns, read at an object type; none for a field it does not
    /// declare.
    pub(super) fn field_result_types(
        &self,
        object_type: &Name,
        field_name: &Name,
    ) -> Arc<Vec<Name>> {
        let key = (object_type.clone(), field_name.clone());
        if let Some(types) = self.result_types_by_field.borrow().get(&key) {
            return types.clone();
        }
        let types = match self.schema.schema().types.get(object_type) {
            Some(ExtendedType::Object(ty)) => match ty.fields.get(field_name) {
                Some(field) => (*self.possible_types(field.ty.inner_named_type())).clone(),
                None => Vec::new(),
            },
            _ => Vec::new(),
        };
        let types = Arc::new(types);
        self.result_types_by_field
            .borrow_mut()
            .insert(key, types.clone());
        types
    }

    /// The object types `types` narrowed by an inline fragment's type condition.
    fn narrow_types(&self, types: &[Name], condition: Option<&Name>) -> Vec<Name> {
        match condition {
            None => types.to_vec(),
            Some(condition) => {
                let admitted = self.possible_types(condition);
                types
                    .iter()
                    .filter(|ty| admitted.contains(ty))
                    .cloned()
                    .collect()
            }
        }
    }

    /// The object types a field named `field_name` returns, read at any of `types`.
    fn child_types(&self, types: &[Name], field_name: &Name) -> Vec<Name> {
        let mut out: IndexSet<Name> = IndexSet::default();
        for object_type in types {
            out.extend(
                self.field_result_types(object_type, field_name)
                    .iter()
                    .cloned(),
            );
        }
        out.into_iter().collect()
    }
}

/// A selection set the key index holds on to: the covering side's own top level, or the selections
/// of a field found in it. Holding the node rather than borrowing its selections is what lets one
/// index serve every fetch reading the same buffer.
#[derive(Clone)]
enum HeldSelections {
    /// The top level of what a fetch can read.
    Buffer(Arc<Vec<Selection>>),
    /// What a field found in it selects.
    BelowField(Node<apollo_compiler::executable::Field>),
}

impl HeldSelections {
    fn selections(&self) -> &[Selection] {
        match self {
            HeldSelections::Buffer(selections) => selections,
            HeldSelections::BelowField(field) => &field.selection_set.selections,
        }
    }
}

/// Where the covering side's keys at one level are found: selection sets, all read at the same
/// object types under the same condition. Copies of one position, which a plan's buffer holds one
/// per fetch mounted there, share both and are merged, so the walk below reads them once.
struct MergedCopies {
    object_types: Vec<Name>,
    condition: BooleanCondition,
    selection_sets: Vec<HeldSelections>,
}

/// Adds `selection_set`, read at `object_types` under `condition`, to the copies sharing both, or
/// as new ones.
fn add_copy(
    copies: &mut Vec<MergedCopies>,
    object_types: Vec<Name>,
    condition: BooleanCondition,
    selection_set: HeldSelections,
) {
    match copies
        .iter_mut()
        .find(|merged| merged.object_types == object_types && merged.condition == condition)
    {
        Some(merged) => merged.selection_sets.push(selection_set),
        None => copies.push(MergedCopies {
            object_types,
            condition,
            selection_sets: vec![selection_set],
        }),
    }
}

/// A field as an occurrence of a response key: the object types and condition it is selected
/// under, its field name, and its selections. Occurrences of one key that agree on all but their
/// selections are one, their fields pooled.
struct KeyOccurrence {
    object_types: Vec<Name>,
    condition: BooleanCondition,
    field_name: Name,
    fields: Vec<Node<apollo_compiler::executable::Field>>,
}

/// The fields under response key `key` that one level of `selections` selects, through its
/// fragments, read at `types` under `condition`; a field whose directives never hold is left out.
fn occurrences_of(
    type_lookup: &SchemaTypeLookup<'_>,
    key: &Name,
    types: &[Name],
    condition: &BooleanCondition,
    selections: &[Selection],
    out: &mut Vec<KeyOccurrence>,
) {
    for selection in selections {
        match selection {
            Selection::Field(field) => {
                if field.response_name() != key {
                    continue;
                }
                let Some(literals) = literals_for_directives(&field.directives) else {
                    continue;
                };
                let mut condition = condition.clone();
                condition.extend(literals);
                match out.iter_mut().find(|occurrence| {
                    occurrence.field_name == field.name
                        && occurrence.object_types == types
                        && occurrence.condition == condition
                }) {
                    Some(occurrence) => occurrence.fields.push(field.clone()),
                    None => out.push(KeyOccurrence {
                        object_types: types.to_vec(),
                        condition,
                        field_name: field.name.clone(),
                        fields: vec![field.clone()],
                    }),
                }
            }
            Selection::InlineFragment(fragment) => {
                let Some(literals) = literals_for_directives(&fragment.directives) else {
                    continue;
                };
                let narrowed = type_lookup.narrow_types(types, fragment.type_condition.as_ref());
                if literals.is_empty() {
                    occurrences_of(
                        type_lookup,
                        key,
                        &narrowed,
                        condition,
                        &fragment.selection_set.selections,
                        out,
                    );
                } else {
                    let mut condition = condition.clone();
                    condition.extend(literals);
                    occurrences_of(
                        type_lookup,
                        key,
                        &narrowed,
                        &condition,
                        &fragment.selection_set.selections,
                        out,
                    );
                }
            }
            Selection::FragmentSpread(_) => {}
        }
    }
}

/// The model's `keysInclude`: whether every key a covered side selects, from the root, is selected
/// by the covering side at the same object types and under conditions covering its own,
/// recursively, below each field only at the object types the plan returns for it. Arguments are
/// not compared; a bare covered field asks for its key alone.
///
/// The covering side is indexed as it is asked: each position, its copies merged by object types and condition,
/// and the occurrences of each key looked up there once. Every entry of an entity fetch is compared
/// with what the fetch can read, and fetches run in parallel read the same buffer, so one index
/// serves them all and the positions they share are walked once.
pub(super) struct ReadableKeyIndex {
    root_types: Vec<Name>,
    positions: std::cell::RefCell<Vec<IndexedPosition>>,
}

/// One position of the covering side: where its keys are found, and what each key asked for so
/// far matched there.
struct IndexedPosition {
    copies: Vec<MergedCopies>,
    matches_by_key: IndexMap<Name, std::rc::Rc<KeyMatch>>,
}

/// What one key matches at a position.
struct KeyMatch {
    /// The object types and condition of each occurrence.
    reached_under: Vec<(Vec<Name>, BooleanCondition)>,
    /// The position below the occurrences.
    position_below: usize,
}

impl ReadableKeyIndex {
    pub(super) fn new(
        type_lookup: &SchemaTypeLookup<'_>,
        root_type: &Name,
        covering: Arc<Vec<Selection>>,
    ) -> Self {
        let root_types = (*type_lookup.possible_types(root_type)).clone();
        let root = IndexedPosition {
            copies: vec![MergedCopies {
                object_types: root_types.clone(),
                condition: Vec::new(),
                selection_sets: vec![HeldSelections::Buffer(covering)],
            }],
            matches_by_key: IndexMap::default(),
        };
        ReadableKeyIndex {
            root_types,
            positions: std::cell::RefCell::new(vec![root]),
        }
    }

    /// What `key` matches at position `node`, looked up once.
    fn lookup(
        &self,
        type_lookup: &SchemaTypeLookup<'_>,
        node: usize,
        key: &Name,
    ) -> std::rc::Rc<KeyMatch> {
        if let Some(found) = self.positions.borrow()[node].matches_by_key.get(key) {
            return found.clone();
        }
        let mut matching = Vec::new();
        {
            let positions = self.positions.borrow();
            for merged in &positions[node].copies {
                for held in &merged.selection_sets {
                    occurrences_of(
                        type_lookup,
                        key,
                        &merged.object_types,
                        &merged.condition,
                        held.selections(),
                        &mut matching,
                    );
                }
            }
        }
        let mut clauses: Vec<(Vec<Name>, BooleanCondition)> = Vec::new();
        let mut below = Vec::new();
        for occurrence in matching {
            let clause = (
                occurrence.object_types.clone(),
                occurrence.condition.clone(),
            );
            if !clauses.contains(&clause) {
                clauses.push(clause);
            }
            let child_types =
                type_lookup.child_types(&occurrence.object_types, &occurrence.field_name);
            for field in occurrence.fields {
                add_copy(
                    &mut below,
                    child_types.clone(),
                    occurrence.condition.clone(),
                    HeldSelections::BelowField(field),
                );
            }
        }
        let mut positions = self.positions.borrow_mut();
        positions.push(IndexedPosition {
            copies: below,
            matches_by_key: IndexMap::default(),
        });
        let found = std::rc::Rc::new(KeyMatch {
            reached_under: clauses,
            position_below: positions.len() - 1,
        });
        positions[node]
            .matches_by_key
            .insert(key.clone(), found.clone());
        found
    }

    /// Whether the covering side includes `covered` by key. `Err` names the first key it does
    /// not.
    pub(super) fn includes(
        &self,
        type_lookup: &SchemaTypeLookup<'_>,
        returned: &ReturnedTypes,
        covered: &[Selection],
    ) -> Result<(), String> {
        self.included_at(
            type_lookup,
            returned,
            0,
            &self.root_types,
            &Vec::new(),
            covered,
        )
    }

    /// One level: `Err` names the first key `covered` selects that the position `node` does not,
    /// at an object type and condition.
    fn included_at(
        &self,
        type_lookup: &SchemaTypeLookup<'_>,
        returned: &ReturnedTypes,
        node: usize,
        types: &[Name],
        condition: &BooleanCondition,
        covered: &[Selection],
    ) -> Result<(), String> {
        for selection in covered {
            match selection {
                Selection::Field(field) => {
                    let Some(literals) = literals_for_directives(&field.directives) else {
                        continue;
                    };
                    let mut extended = condition.clone();
                    extended.extend(literals);
                    let Some(reached) = canonical_boolean_condition(&extended) else {
                        continue;
                    };
                    let key = field.response_name();
                    let found = self.lookup(type_lookup, node, key);
                    for object_type in types {
                        let clauses: Vec<BooleanCondition> = found
                            .reached_under
                            .iter()
                            .filter(|(clause_types, _)| clause_types.contains(object_type))
                            .map(|(_, clause)| clause.clone())
                            .collect();
                        if !boolean_condition_covered_by(&reached, &clauses) {
                            return Err(format!("`{key}` is not read on {object_type}"));
                        }
                    }
                    if !field.selection_set.selections.is_empty() {
                        let returned_children: Vec<Name> = type_lookup
                            .child_types(types, &field.name)
                            .into_iter()
                            .filter(|child| {
                                types.iter().any(|object_type| {
                                    returned.returns(object_type, &field.name, child)
                                })
                            })
                            .collect();
                        self.included_at(
                            type_lookup,
                            returned,
                            found.position_below,
                            &returned_children,
                            &extended,
                            &field.selection_set.selections,
                        )
                        .map_err(|reason| format!("in `{key}`: {reason}"))?;
                    }
                }
                Selection::InlineFragment(fragment) => {
                    let Some(literals) = literals_for_directives(&fragment.directives) else {
                        continue;
                    };
                    let mut extended = condition.clone();
                    extended.extend(literals);
                    self.included_at(
                        type_lookup,
                        returned,
                        node,
                        &type_lookup.narrow_types(types, fragment.type_condition.as_ref()),
                        &extended,
                        &fragment.selection_set.selections,
                    )?;
                }
                Selection::FragmentSpread(_) => {}
            }
        }
        Ok(())
    }
}
