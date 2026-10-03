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
//! The model answers with a `Bool`, which states the property and says nothing about a failure.
//! [`RequirementMismatch`] is the missing half, the way `query_compare::error` is for
//! `includesBool`: the response keys descended into, and which of the level's checks failed at
//! the end of them.
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

use std::fmt;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
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
) -> Option<&'a ast::FieldDefinition> {
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

//==================================================================================================
// Why an entry does not declare what a subgraph demands

/// Where the two field sets part company, and what parted them.
///
/// The model decides this comparison with a `Bool`, which is enough to state the property and not
/// enough to act on a failure — the same gap `query_compare::error` closes for `includesBool`.
/// [`path`](Self::path) is the descent by response key, outermost first; [`reason`](Self::reason)
/// is what failed at the end of it.
#[derive(Debug)]
pub(super) struct RequirementMismatch {
    path: Vec<Name>,
    reason: RequirementReason,
}

/// What failed at the end of a [`RequirementMismatch`]'s path. One variant per check
/// `condition_matches_requirement_at` makes at a key.
#[derive(Debug)]
enum RequirementReason {
    /// The subgraph demands this key and the entry does not declare it.
    NotDeclared { key: Name },

    /// The entry declares this key and the subgraph does not demand it. A `requires` entry is
    /// matched to a `@key` exactly, so declaring more is as much a mismatch as declaring less.
    NotDemanded { key: Name },

    /// One side selects the key with nothing under it and the other selects into it.
    LeafDisagreement { key: Name, leaf_in_entry: bool },

    /// The entry reaches the key at object types the demand does not, so it asks for the field
    /// somewhere the subgraph never promised it.
    TypesNotCovered {
        key: Name,
        entry: GroundTypes,
        demand: GroundTypes,
    },
}

/// The object types a side reaches a key at, as the reasons above print them.
fn render_types(types: &GroundTypes) -> String {
    match types {
        None => "every type".to_string(),
        Some(names) if names.is_empty() => "no type".to_string(),
        Some(names) => format!(
            "{{{}}}",
            names
                .iter()
                .map(Name::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

impl fmt::Display for RequirementMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for key in &self.path {
            writeln!(f, "under response key: {key}")?;
        }
        write!(f, "--> ")?;
        match &self.reason {
            RequirementReason::NotDeclared { key } => {
                write!(
                    f,
                    "the subgraph demands `{key}`, which the entry does not declare"
                )
            }
            RequirementReason::NotDemanded { key } => {
                write!(
                    f,
                    "the entry declares `{key}`, which the subgraph does not demand"
                )
            }
            RequirementReason::LeafDisagreement { key, leaf_in_entry } => {
                let (bare, nested) = if *leaf_in_entry {
                    ("the entry", "the subgraph")
                } else {
                    ("the subgraph", "the entry")
                };
                write!(
                    f,
                    "`{key}` is selected bare by {bare} and selected into by {nested}"
                )
            }
            RequirementReason::TypesNotCovered { key, entry, demand } => write!(
                f,
                "the entry reaches `{key}` at {}, which the subgraph's {} does not cover",
                render_types(entry),
                render_types(demand)
            ),
        }
    }
}

impl RequirementMismatch {
    fn at(reason: RequirementReason) -> RequirementMismatch {
        RequirementMismatch {
            path: Vec::new(),
            reason,
        }
    }

    /// Records that this failure was found under `key`, one level further out.
    fn under(mut self, key: &Name) -> RequirementMismatch {
        self.path.insert(0, key.clone());
        self
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
) -> Result<(), RequirementMismatch> {
    condition_matches_requirement_at(schema, &None, requirement, condition)
}

/// `condition_matches_requirement`, at a known position. `reached_at` is the object types the
/// enclosing field can return, which is what a type condition written here is read against.
fn condition_matches_requirement_at(
    schema: &ValidFederationSchema,
    reached_at: &GroundTypes,
    requirement: &[&requires_selection::Selection],
    condition: &[&requires_selection::Selection],
) -> Result<(), RequirementMismatch> {
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

    for key in &keys {
        let in_requirement = requirement_level.iter().any(|entry| entry.key == *key);
        let in_condition = condition_level.iter().any(|entry| entry.key == *key);
        match (in_requirement, in_condition) {
            (false, true) => {
                return Err(RequirementMismatch::at(RequirementReason::NotDeclared {
                    key: key.clone(),
                }));
            }
            (true, false) => {
                return Err(RequirementMismatch::at(RequirementReason::NotDemanded {
                    key: key.clone(),
                }));
            }
            _ => {}
        }
        let leaf_in_entry = reaches_leaf(key, &requirement_level);
        if leaf_in_entry != reaches_leaf(key, &condition_level) {
            return Err(RequirementMismatch::at(
                RequirementReason::LeafDisagreement {
                    key: key.clone(),
                    leaf_in_entry,
                },
            ));
        }
        let entry_types = reached_types(key, &requirement_level);
        let demand_types = reached_types(key, &condition_level);
        if !types_covered(&entry_types, &demand_types) {
            return Err(RequirementMismatch::at(
                RequirementReason::TypesNotCovered {
                    key: key.clone(),
                    entry: entry_types,
                    demand: demand_types,
                },
            ));
        }
        // Terminates because the merge is strictly smaller: the key was reached by one of the
        // two levels. This is what the model needs fuel for.
        condition_matches_requirement_at(
            schema,
            &child_types(schema, key, &union_types(entry_types, demand_types)),
            &selected_under(key, &requirement_level),
            &selected_under(key, &condition_level),
        )
        .map_err(|mismatch| mismatch.under(key))?;
    }
    Ok(())
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

//==================================================================================================
// Conflicting demands between two `@requires`

/// Reports two demands on one response key that are different resolver calls — FED-504.
///
/// [`requires_half`] concatenates the `@requires` field sets of every field a fetch selects on the
/// entity; it does not merge them, because nothing here can merge two calls that disagree. When
/// they do disagree the fetch can only make one of them, and one of the two fields is fed data it
/// did not ask for.
/// - Nothing else in this checker can see it. The plan's `requires` entry has had its arguments
///   dropped by `trim_requires_selection_set`, and the concatenation is handed to `query_compare`
///   as one document, where the field-merging rule the model assumes makes the first occurrence
///   stand for the rest.
/// - Two demands under conditions that cannot both hold are not in conflict, so the comparison is
///   made only where the positions overlap.
/// - Off by default, as [`CheckerOptions::check_requires_conflict`] says: the planner emits these
///   plans today.
pub(super) fn check_requires_conflict(
    schema: &ValidFederationSchema,
    selections: &[Selection],
) -> Result<(), ComparisonError> {
    check_no_conflicting_demands(schema, selections, &[], &[])
}

fn check_no_conflicting_demands(
    schema: &ValidFederationSchema,
    selections: &[Selection],
    guards: &[Name],
    path: &[Name],
) -> Result<(), ComparisonError> {
    let mut demands: Vec<Demand<'_>> = Vec::new();
    collect_demands(schema, selections, guards, &mut demands);

    let mut keys: Vec<&Name> = Vec::new();
    for demand in &demands {
        let key = demand.field.response_key();
        if !keys.contains(&key) {
            keys.push(key);
        }
    }

    for key in keys {
        let here: Vec<&Demand<'_>> = demands
            .iter()
            .filter(|demand| demand.field.response_key() == key)
            .collect();
        for (index, left) in here.iter().enumerate() {
            for right in &here[index + 1..] {
                let left_call = resolver_call(left.field);
                let right_call = resolver_call(right.field);
                if left_call != right_call && positions_overlap(&left.reached_at, &right.reached_at)
                {
                    return Err(ComparisonError::new(format!(
                        "two `@requires` demand `{key}` with different calls, and the fetch can \
                         make only one of them:\n  at:        {}\n  one wants: {left_call}\n  \
                         the other: {right_call}",
                        render_path(path)
                    )));
                }
            }
        }
        // The demands agree here, so what is under them is one position and can conflict there
        // in turn.
        let below: Vec<Selection> = here
            .iter()
            .flat_map(|demand| demand.field.selection_set.selections.iter().cloned())
            .collect();
        if !below.is_empty() {
            let mut deeper = path.to_vec();
            deeper.push(key.clone());
            check_no_conflicting_demands(schema, &below, &[], &deeper)?;
        }
    }
    Ok(())
}

/// One demand at one level: the object types it is made at, and the field making it.
struct Demand<'a> {
    reached_at: GroundTypes,
    field: &'a executable::Field,
}

/// The fields a selection set demands at one level. An inline fragment narrows the position of
/// what is under it without being a demand of its own, which is how a response shape flattens it.
fn collect_demands<'a>(
    schema: &ValidFederationSchema,
    selections: &'a [Selection],
    guards: &[Name],
    out: &mut Vec<Demand<'a>>,
) {
    for selection in selections {
        match selection {
            Selection::Field(field) => out.push(Demand {
                reached_at: admitted_types(schema, guards),
                field,
            }),
            Selection::InlineFragment(fragment) => {
                let mut narrowed = guards.to_vec();
                if let Some(type_condition) = &fragment.type_condition {
                    narrowed.push(type_condition.clone());
                }
                collect_demands(schema, &fragment.selection_set.selections, &narrowed, out);
            }
            // Spreads are inlined before a fetch's selections reach here.
            Selection::FragmentSpread(_) => {}
        }
    }
}

/// Can two demands ever be made at the same runtime type? An unrestricted position meets
/// everything; two restricted ones meet where their object types do.
fn positions_overlap(left: &GroundTypes, right: &GroundTypes) -> bool {
    match (left, right) {
        (None, _) | (_, None) => true,
        (Some(left), Some(right)) => left.iter().any(|name| right.contains(name)),
    }
}

fn render_path(path: &[Name]) -> String {
    if path.is_empty() {
        "the entity".to_string()
    } else {
        path.iter().map(Name::as_str).collect::<Vec<_>>().join(".")
    }
}

/// A field's resolver call, rendered so that two are equal exactly when the call is the same.
/// Arguments are a set keyed by name, as GraphQL gives their order no meaning.
fn resolver_call(field: &executable::Field) -> String {
    let mut arguments: Vec<String> = field
        .arguments
        .iter()
        .map(|argument| format!("{}: {}", argument.name, one_line(&argument.value)))
        .collect();
    arguments.sort();
    if arguments.is_empty() {
        field.name.to_string()
    } else {
        format!("{}({})", field.name, arguments.join(", "))
    }
}

/// A value the way it is written in a query. `Display` breaks a list or object over several
/// lines, which reads badly in a message naming two calls one after the other.
fn one_line(value: &ast::Value) -> String {
    let collapsed = value
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    collapsed
        .replace(", ]", "]")
        .replace(", }", "}")
        .replace("[ ", "[")
        .replace(" ]", "]")
        .replace("{ ", "{")
        .replace(" }", "}")
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

#[cfg(test)]
mod tests {
    use apollo_compiler::name;
    use apollo_compiler::schema::Schema;

    use super::*;

    /// `Review` is an object and `reviewsForLocation` returns `[Review]!`, so `... on Review`
    /// written there narrows nothing -- the shape Apollo's own FlyBy demo `@requires` takes.
    /// `Node` is the abstract counterpart, where a condition can be vacuous or genuine.
    const SCHEMA: &str = r#"
        type Query { locations: [Location!]!, things: [Node!]! }
        type Location {
          id: ID!
          reviewsForLocation(kinds: [Int!]): [Review]!
          reviews: [Review!]!
        }
        type Review { id: ID!, rating: Int, tag(n: Int): String }
        interface Node { id: ID!, tag(n: Int): String }
        type Thing implements Node { id: ID!, tag(n: Int): String }
        type Other implements Node { id: ID!, tag(n: Int): String }
    "#;

    fn schema() -> ValidFederationSchema {
        let schema = Schema::parse_and_validate(SCHEMA, "schema.graphql").unwrap();
        ValidFederationSchema::new(schema).unwrap()
    }

    /// A `requires` entry, written the way `key_half` and `requires_half` build one: under the
    /// entity type's own condition, which is what puts level 0 at a known position.
    fn field_set(schema: &ValidFederationSchema, text: &str) -> Vec<requires_selection::Selection> {
        let text = format!("... on Query {{ {text} }}");
        let parsed = super::super::subgraph::parse_field_set(schema, &name!("Query"), &text)
            .expect("valid field set");
        to_requires_field_set(&parsed)
    }

    /// Does the subgraph's demand match the entry? `Err` renders as it would inside a rejection.
    fn compare(entry: &str, demand: &str) -> Result<(), String> {
        let schema = schema();
        let entry = field_set(&schema, entry);
        let demand = field_set(&schema, demand);
        condition_matches_requirement(
            &schema,
            &entry.iter().collect::<Vec<_>>(),
            &demand.iter().collect::<Vec<_>>(),
        )
        .map_err(|mismatch| mismatch.to_string())
    }

    //==============================================================================================
    // Grounding a type condition against its position

    /// `... on Review` inside a field of type `[Review]!` admits everything the position can
    /// hold, so it selects what its body selects. Reading it as a narrowing rejected a plan the
    /// router executes correctly.
    #[test]
    fn a_vacuous_condition_in_the_demand_is_matched() {
        compare(
            "locations { reviewsForLocation { id rating } }",
            "locations { reviewsForLocation { ... on Review { id rating } } }",
        )
        .unwrap();
    }

    /// The same at an abstract position: a condition naming the whole interface narrows nothing.
    #[test]
    fn a_condition_naming_the_position_itself_is_matched() {
        compare("things { id }", "things { ... on Node { id } }").unwrap();
    }

    /// A condition that really does narrow still fails, in the same direction as before.
    #[test]
    fn a_real_narrowing_in_the_demand_is_not_matched() {
        assert_eq!(
            compare("things { id }", "things { ... on Thing { id } }").unwrap_err(),
            "under response key: things\n\
             --> the entry reaches `id` at {Other, Thing}, which the subgraph's {Thing} does not cover"
        );
    }

    /// An entry narrower than the demand is matched: asking for less than the subgraph promises
    /// at a position is not a mismatch.
    #[test]
    fn a_narrower_entry_is_matched() {
        compare("things { ... on Thing { id } }", "things { id }").unwrap();
    }

    //==============================================================================================
    // What a mismatch says

    #[test]
    fn a_key_the_entry_omits_is_named_with_its_path() {
        assert_eq!(
            compare(
                "locations { reviewsForLocation { id } }",
                "locations { reviewsForLocation { id rating } }",
            )
            .unwrap_err(),
            "under response key: locations\n\
             under response key: reviewsForLocation\n\
             --> the subgraph demands `rating`, which the entry does not declare"
        );
    }

    /// An entry is matched to a `@key` exactly, so declaring more is as much a mismatch as
    /// declaring less.
    #[test]
    fn a_key_the_subgraph_does_not_demand_is_named() {
        assert_eq!(
            compare(
                "locations { reviewsForLocation { id rating } }",
                "locations { reviewsForLocation { id } }",
            )
            .unwrap_err(),
            "under response key: locations\n\
             under response key: reviewsForLocation\n\
             --> the entry declares `rating`, which the subgraph does not demand"
        );
    }

    //==============================================================================================
    // Conflicting demands between two `@requires`

    /// Two `@requires` field sets, concatenated the way `requires_half` concatenates them.
    fn demands(left: &str, right: &str) -> Result<(), String> {
        let schema = schema();
        let mut selections = Vec::new();
        for text in [left, right] {
            let text = format!("... on Query {{ {text} }}");
            selections.extend(
                super::super::subgraph::parse_field_set(&schema, &name!("Query"), &text)
                    .expect("valid field set"),
            );
        }
        check_requires_conflict(&schema, &selections).map_err(|e| e.description().to_string())
    }

    /// The shape FED-504 describes: two `@requires` on one entity naming the same key with
    /// different arguments. A fetch can make only one of the two calls, so one of the two fields
    /// is fed data it did not ask for.
    #[test]
    fn two_requires_demanding_different_calls_conflict() {
        let error = demands(
            "locations { reviewsForLocation(kinds: [47, 141]) { id } }",
            "locations { reviewsForLocation(kinds: [141]) { id } }",
        )
        .expect_err("should conflict");
        assert!(
            error.contains("two `@requires` demand `reviewsForLocation`")
                && error.contains("reviewsForLocation(kinds: [47, 141])")
                && error.contains("reviewsForLocation(kinds: [141])"),
            "unexpected message: {error}"
        );
    }

    /// The conflict is reported at the position it happens, however deep.
    #[test]
    fn a_conflict_below_the_top_level_names_its_path() {
        let error = demands(
            "locations { reviews { tag(n: 1) } }",
            "locations { reviews { tag(n: 2) } }",
        )
        .expect_err("should conflict");
        assert!(
            error.contains("at:        locations.reviews"),
            "unexpected message: {error}"
        );
    }

    /// Demands that make the same call and differ only in what they select below are merged.
    #[test]
    fn two_requires_making_the_same_call_do_not_conflict() {
        demands(
            "locations { reviewsForLocation(kinds: [141]) { id } }",
            "locations { reviewsForLocation(kinds: [141]) { rating } }",
        )
        .expect("no conflict");
        demands(
            "locations { reviewsForLocation { id } }",
            "locations { reviewsForLocation { rating } }",
        )
        .expect("no conflict");
    }

    /// Two demands under type conditions that cannot both hold are never made together, so they
    /// are free to differ.
    #[test]
    fn demands_at_disjoint_positions_do_not_conflict() {
        demands(
            "things { ... on Thing { tag(n: 1) } }",
            "things { ... on Other { tag(n: 2) } }",
        )
        .expect("no conflict");
    }

    /// Positions that do overlap are still compared: an abstract condition and a concrete one
    /// that it admits are the same position for a type they share.
    #[test]
    fn demands_at_overlapping_positions_conflict() {
        demands(
            "things { ... on Node { tag(n: 1) } }",
            "things { ... on Thing { tag(n: 2) } }",
        )
        .expect_err("should conflict");
    }

    /// Entries and cases are matched many-to-many, so the comparison is asked about pairs no
    /// validated document could produce -- one selecting a key bare where the other selects into
    /// it. Built by hand for that reason.
    #[test]
    fn selecting_a_key_bare_against_selecting_into_it_is_named() {
        fn field(
            name: Name,
            selections: Vec<requires_selection::Selection>,
        ) -> requires_selection::Selection {
            requires_selection::Selection::Field(requires_selection::Field {
                alias: None,
                name,
                selections,
            })
        }
        let entry = [field(name!("x"), vec![])];
        let demand = [field(name!("x"), vec![field(name!("y"), vec![])])];
        let mismatch = condition_matches_requirement(
            &schema(),
            &entry.iter().collect::<Vec<_>>(),
            &demand.iter().collect::<Vec<_>>(),
        )
        .unwrap_err();
        assert_eq!(
            mismatch.to_string(),
            "--> `x` is selected bare by the entry and selected into by the subgraph"
        );
    }
}
