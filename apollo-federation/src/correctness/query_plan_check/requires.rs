//! Comparing a fetch's declared `requires` against what its subgraph actually demands.
//!
//! Port of the `conditionMatchesRequirement` half of `Apollo/Definitions/QueryPlanSoundness.lean`.
//!
//! A field set is read **one level at a time**: [`level_entries`] gives that level's response keys
//! with the object types each is reached at and what is selected under it, and
//! [`condition_matches_requirement`] compares two field sets level by level, recursing on the
//! merged subselections of each key. No root-to-leaf paths are materialized.
//!
//! The model carries an explicit fuel here, one unit per level, purely to satisfy Lean's
//! termination checker — its own notes say "an implementation should simply loop". This loops.
//!
//! # What a subgraph demands
//!
//! [`key_half`] and [`requires_half`] together are the model's `entityFetchRequirement`:
//! `__typename` and a `@key`'s own fields, plus the `@requires` field sets of the fields the fetch
//! selects on the entity. They are split because only `key.fields` varies across the keys a fetch
//! is checked against, so the other half is computed once per entity case.
//!
//! The halves sit under different type conditions — the key at `require_type`, how the supergraph
//! declares the entity, and the `@requires` at `entity_type`, how the subgraph does. The two
//! differ when the subgraph declares the entity as an interface object.
//!
//! A `@key` need not typecheck at the type it is read against, and that is ordinary rather than
//! wrong. Entries and cases are matched many-to-many, so the caller asks about pairs the planner
//! never meant to go together — one entity type's key against another's entry — and two entity
//! types under one interface need not share a key at all. The model converts a field set
//! syntactically and lets the comparison reject such a pair; parsing is how this port builds a
//! requirement at all, so failing to parse is that same rejection, reported as an `Err` the caller
//! reads as a non-match.

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::executable;
use apollo_compiler::executable::Selection;

use super::ComparisonError;
use super::selections::admitted_types;
use super::selections::possible_types;
use super::selections::under_directives;
use super::selections::wrap_non_empty;
use super::subgraph::Subgraph;
use crate::query_plan::requires_selection;
use crate::schema::ValidFederationSchema;

//==================================================================================================
// Checking FetchNode.requires field -- nominal required response names
//==================================================================================================

/// The object types a written type condition grounds to, sorted so the result is canonical.
/// `None` means unrestricted.
type GroundTypes = Option<Vec<Name>>;

/// One level of a field set: each response key, the object types it is reached at there, and what
/// is selected under it.
///
/// Inline fragments contribute their type condition to the entries below without an entry of their
/// own; a field starts a new level, so guards do not cross it.
struct LevelEntry<'a> {
    key: Name,
    reached_at: GroundTypes,
    selected: Vec<&'a requires_selection::Selection>,
}

fn level_entries<'a>(
    schema: &ValidFederationSchema,
    reached_at: &GroundTypes,
    field_set: &[&'a requires_selection::Selection],
    out: &mut Vec<LevelEntry<'a>>,
) {
    for selection in field_set {
        match selection {
            requires_selection::Selection::Field(field) => out.push(LevelEntry {
                key: field.alias.clone().unwrap_or_else(|| field.name.clone()),
                reached_at: reached_at.clone(),
                selected: field.selections.iter().collect(),
            }),
            requires_selection::Selection::InlineFragment(fragment) => {
                let mut narrowed = reached_at.clone();
                if let Some(type_condition) = &fragment.type_condition {
                    narrowed = intersect_types(
                        narrowed,
                        admitted_types(schema, std::slice::from_ref(type_condition)),
                    );
                }
                let nested: Vec<&requires_selection::Selection> =
                    fragment.selections.iter().collect();
                level_entries(schema, &narrowed, &nested, out);
            }
        }
    }
}

/// The intersection of two type conditions. `None` is unrestricted, and so is absorbed.
fn intersect_types(left: GroundTypes, right: GroundTypes) -> GroundTypes {
    match (left, right) {
        (None, right) => right,
        (left, None) => left,
        (Some(left), Some(right)) => {
            Some(left.into_iter().filter(|ty| right.contains(ty)).collect())
        }
    }
}

/// The object types the position under `key` reaches, given the types `key` itself is reached at.
///
/// A field's own type is what bounds its sub-selection, so a type condition written there that
/// admits all of it narrows nothing. Without this the two sides are compared at different
/// positions: an unrestricted sub-selection reads as wider than one under a condition that is
/// vacuous where it sits. `None` — the position is unknown — keeps the older, unbounded reading.
fn child_types(
    schema: &ValidFederationSchema,
    key: &Name,
    reached_at: &GroundTypes,
) -> GroundTypes {
    let Some(parents) = reached_at else {
        return None;
    };
    let mut child: Vec<Name> = Vec::new();
    for parent in parents {
        let Some(definition) = lookup_field(schema, parent, key) else {
            // A key the schema does not place here bounds nothing.
            return None;
        };
        for name in possible_types(schema, definition.ty.inner_named_type()) {
            if !child.contains(&name) {
                child.push(name);
            }
        }
    }
    child.sort();
    Some(child)
}

/// A field definition on an object or interface type. `__typename` is selectable everywhere and
/// declared nowhere; it is a leaf, so it never bounds a sub-selection.
fn lookup_field<'a>(
    schema: &'a ValidFederationSchema,
    parent_type: &Name,
    field_name: &Name,
) -> Option<&'a apollo_compiler::ast::FieldDefinition> {
    match schema.schema().types.get(parent_type)? {
        apollo_compiler::schema::ExtendedType::Object(ty) => {
            ty.fields.get(field_name).map(|field| &***field)
        }
        apollo_compiler::schema::ExtendedType::Interface(ty) => {
            ty.fields.get(field_name).map(|field| &***field)
        }
        _ => None,
    }
}

fn level_keys(entries: &[LevelEntry<'_>]) -> Vec<Name> {
    let mut keys: Vec<Name> = Vec::new();
    for entry in entries {
        if !keys.contains(&entry.key) {
            keys.push(entry.key.clone());
        }
    }
    keys
}

/// Does a level reach `key` with nothing under it — a leaf of the key tree?
fn reaches_leaf(key: &Name, entries: &[LevelEntry<'_>]) -> bool {
    entries
        .iter()
        .any(|entry| entry.key == *key && entry.selected.is_empty())
}

/// The union of two type conditions. `None` is unrestricted, and so absorbs.
fn union_types(left: GroundTypes, right: GroundTypes) -> GroundTypes {
    let (Some(left), Some(right)) = (left, right) else {
        return None;
    };
    let mut union = left;
    for name in right {
        if !union.contains(&name) {
            union.push(name);
        }
    }
    union.sort();
    Some(union)
}

/// The object types a level reaches `key` at, over all of its occurrences. A key the level does
/// not reach is reached at no type.
fn reached_types(key: &Name, entries: &[LevelEntry<'_>]) -> GroundTypes {
    let mut reached: GroundTypes = Some(Vec::new());
    for entry in entries {
        if entry.key == *key {
            reached = union_types(entry.reached_at.clone(), reached);
        }
    }
    reached
}

/// What a level selects under `key`, merged over its occurrences.
fn selected_under<'a>(
    key: &Name,
    entries: &[LevelEntry<'a>],
) -> Vec<&'a requires_selection::Selection> {
    entries
        .iter()
        .filter(|entry| entry.key == *key)
        .flat_map(|entry| entry.selected.iter().copied())
        .collect()
}

/// Is one type condition no narrower than another? An unrestricted condition covers everything; a
/// restricted one cannot cover the unrestricted.
fn types_covered(narrower: &GroundTypes, wider: &GroundTypes) -> bool {
    match (narrower, wider) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(narrower), Some(wider)) => narrower.iter().all(|name| wider.contains(name)),
    }
}

/// Does what a subgraph demands match what the fetch declares it requires?
///
/// At each level both must reach the same response keys, agree on which are leaves, and the
/// requirement's types must be covered by the condition's; then recurse on merged subselections.
pub(super) fn condition_matches_requirement(
    schema: &ValidFederationSchema,
    requirement: &[&requires_selection::Selection],
    condition: &[&requires_selection::Selection],
) -> bool {
    condition_matches_requirement_at(schema, &None, requirement, condition)
}

/// `condition_matches_requirement`, at a known position. `reached_at` is the object types the
/// enclosing field can return, which is what a type condition written here is read against.
fn condition_matches_requirement_at(
    schema: &ValidFederationSchema,
    reached_at: &GroundTypes,
    requirement: &[&requires_selection::Selection],
    condition: &[&requires_selection::Selection],
) -> bool {
    let mut requirement_level = Vec::new();
    level_entries(schema, reached_at, requirement, &mut requirement_level);
    let mut condition_level = Vec::new();
    level_entries(schema, reached_at, condition, &mut condition_level);

    let mut keys = level_keys(&requirement_level);
    for key in level_keys(&condition_level) {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }

    keys.iter().all(|key| {
        let in_requirement = requirement_level.iter().any(|entry| entry.key == *key);
        let in_condition = condition_level.iter().any(|entry| entry.key == *key);
        in_requirement == in_condition
            && reaches_leaf(key, &requirement_level) == reaches_leaf(key, &condition_level)
            && types_covered(
                &reached_types(key, &requirement_level),
                &reached_types(key, &condition_level),
            )
            // Terminates because the merge is strictly smaller: the key was reached by one of
            // the two levels. This is what the model needs fuel for.
            && condition_matches_requirement_at(
                schema,
                &child_types(
                    schema,
                    key,
                    &union_types(
                        reached_types(key, &requirement_level),
                        reached_types(key, &condition_level),
                    ),
                ),
                &selected_under(key, &requirement_level),
                &selected_under(key, &condition_level),
            )
    })
}

/// A selection read back as a field-set selection: its response key becomes the name, and its
/// arguments and directives are dropped. Field sets have no aliases.
pub(super) fn to_requires_field_set(
    selections: &[Selection],
) -> Vec<requires_selection::Selection> {
    selections
        .iter()
        .filter_map(|selection| match selection {
            Selection::Field(field) => Some(requires_selection::Selection::Field(
                requires_selection::Field {
                    alias: None,
                    name: field.response_key().clone(),
                    selections: to_requires_field_set(&field.selection_set.selections),
                },
            )),
            Selection::InlineFragment(fragment) => Some(
                requires_selection::Selection::InlineFragment(requires_selection::InlineFragment {
                    type_condition: fragment.type_condition.clone(),
                    selections: to_requires_field_set(&fragment.selection_set.selections),
                }),
            ),
            // Spreads are inlined before a fetch's selections reach here.
            Selection::FragmentSpread(_) => None,
        })
        .collect()
}

//==================================================================================================
// Extracting required field sets from subgraph schema and entity type
//==================================================================================================

/// The `@key` half of what a subgraph demands before it will resolve an entity: `__typename` and
/// the key's own fields.
///
/// `__typename` is included because every entity representation carries it, as
/// `compute_response_shape_for_field_set_with_typename` has it. `Err` means the key does not
/// typecheck at `require_type`, which is a pair that does not match rather than a malformed plan;
/// see the module docs.
pub(super) fn key_half(
    supergraph_schema: &ValidFederationSchema,
    entity_type: &Name,
    key_fields: &str,
) -> Result<Vec<Selection>, ComparisonError> {
    let fields = format!("__typename {key_fields}");
    let selections = super::subgraph::parse_field_set(supergraph_schema, entity_type, &fields)?;
    Ok(wrap_non_empty(
        |selections| inline_fragment(entity_type.clone(), selections),
        selections,
    ))
}

/// The `@requires` half: the `@requires` field sets of the fields the fetch selects on the entity.
pub(super) fn requires_half(
    supergraph_schema: &ValidFederationSchema,
    subgraph: &Subgraph<'_>,
    entity_type: &Name,
    entity_selections: &[Selection],
) -> Result<Vec<Selection>, ComparisonError> {
    let selections =
        required_selections(supergraph_schema, subgraph, entity_type, entity_selections)?;
    Ok(wrap_non_empty(
        |selections| inline_fragment(entity_type.clone(), selections),
        selections,
    ))
}

/// The `@requires` field sets of the fields a fetch selects on an entity, each kept under the
/// directives of the selection that carries it — as `collect_require_condition` inherits each
/// variant's Boolean clause.
///
/// Looked up in the subgraph by field name, parsed against the supergraph at `entity_type`.
fn required_selections(
    supergraph_schema: &ValidFederationSchema,
    subgraph: &Subgraph<'_>,
    entity_type: &Name,
    entity_selections: &[Selection],
) -> Result<Vec<Selection>, ComparisonError> {
    let mut out = Vec::new();
    for selection in entity_selections {
        let (directives, required) = match selection {
            Selection::Field(field) => {
                let Some(field_set) = subgraph.requires(entity_type, &field.name)? else {
                    continue;
                };
                (
                    &field.directives,
                    super::subgraph::parse_field_set(supergraph_schema, entity_type, &field_set)?,
                )
            }
            Selection::InlineFragment(fragment) => (
                &fragment.directives,
                required_selections(
                    supergraph_schema,
                    subgraph,
                    entity_type,
                    &fragment.selection_set.selections,
                )?,
            ),
            // Spreads are inlined before a fetch's selections reach here.
            Selection::FragmentSpread(_) => continue,
        };
        out.extend(under_directives(directives, entity_type, required));
    }
    Ok(out)
}

fn inline_fragment(type_condition: Name, selections: Vec<Selection>) -> Selection {
    let mut selection_set = executable::SelectionSet::new(type_condition.clone());
    selection_set.selections = selections;
    Selection::InlineFragment(Node::new(executable::InlineFragment {
        type_condition: Some(type_condition),
        directives: Default::default(),
        selection_set,
    }))
}
