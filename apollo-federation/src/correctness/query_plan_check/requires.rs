//! What an entity fetch's subgraph demands, and how a `requires` entry is read.
//!
//! Port of the `entityFetchRequirement` and representation halves of
//! `Apollo/Definitions/QueryPlanSoundness.lean`.
//!
//! # What a subgraph demands
//!
//! [`entity_fetch_requirement`] is the model's `entityFetchRequirement`: `__typename` and a
//! `@key`'s own fields, plus the `@requires` field sets of the fields the fetch selects on the
//! entity, each under the conditions of the selection carrying it, all written at the object type
//! the representation is sent for. For an interface object that object type is an implementation
//! of the entity type, which has every field the entity type declares.
//!
//! # The two readings of an entry
//!
//! An entry is read from the in-memory result under its aliases ([`Reading::AsRead`]) and sent
//! under its fields' own names ([`Reading::AsSent`]), each inline fragment under the
//! `@skip`/`@include` conditions it carries. Arguments are never read: the demand supplies them.

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable;
use apollo_compiler::executable::Selection;

use super::ComparisonError;
use super::selections::admitted_types;
use super::selections::condition_directives;
use super::selections::inline_fragment_under;
use super::selections::under_directives;
use super::subgraph::Subgraph;
use crate::query_plan::requires_selection;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;

/// The object types a written type condition grounds to, sorted so the result is canonical.
/// `None` means unrestricted.
type GroundTypes = Option<Vec<Name>>;

//==================================================================================================
// The two readings of a `requires` entry
//==================================================================================================

/// Which key each field of an entry is taken under.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Reading {
    /// The key `execute_selection_set` reads it from: its alias if it has one.
    AsRead,
    /// Its own name, which is what the subgraph reads it as once the rewrites have run.
    AsSent,
}

/// A `requires` entry read as an ordinary selection, under one reading of its keys.
///
/// Arguments are dropped — Rust's `requires_selection::Field` has none, and the demand is where
/// what a key must hold is read from. `Err` means the entry names a field the schema does not
/// place where it sits, which is a malformed plan.
pub(super) fn entry_selections(
    schema: &ValidFederationSchema,
    parent_type: &Name,
    reading: Reading,
    selections: &[requires_selection::Selection],
) -> Result<Vec<Selection>, ComparisonError> {
    let mut out = Vec::new();
    for selection in selections {
        match selection {
            requires_selection::Selection::Field(field) => {
                let definition = field_definition(schema, parent_type, &field.name)?;
                let mut built = executable::Field::new(field.name.clone(), definition);
                if reading == Reading::AsRead {
                    built.alias = field.alias.clone();
                }
                if !field.selections.is_empty() {
                    let child = built.ty().inner_named_type().clone();
                    built = built.with_selections(entry_selections(
                        schema,
                        &child,
                        reading,
                        &field.selections,
                    )?);
                }
                out.push(built.into());
            }
            requires_selection::Selection::InlineFragment(fragment) => {
                let inner = fragment.type_condition.as_ref().unwrap_or(parent_type);
                out.push(inline_fragment_under(
                    fragment.type_condition.as_ref(),
                    parent_type,
                    &condition_directives(&fragment.conditions),
                    entry_selections(schema, inner, reading, &fragment.selections)?,
                ));
            }
        }
    }
    Ok(out)
}

/// The definition a field is read against. `Err` is the entry naming a field the schema does not
/// place here, which the caller reads as a malformed plan rather than a mismatch.
fn field_definition(
    schema: &ValidFederationSchema,
    parent_type: &Name,
    field_name: &Name,
) -> Result<Node<ast::FieldDefinition>, ComparisonError> {
    let missing = || {
        ComparisonError::new(format!(
            "`requires` entry selects `{field_name}`, which `{parent_type}` does not have"
        ))
    };
    let parent: CompositeTypeDefinitionPosition = schema
        .get_type(parent_type)
        .map_err(|_| missing())?
        .try_into()
        .map_err(|_| missing())?;
    parent
        .field(field_name.clone())
        .map_err(|_| missing())?
        .get(schema.schema())
        .cloned()
        .map_err(|_| missing())
}

/// Two selections of the same shape with the same response keys throughout.
///
/// Renaming changes response keys and nothing else, so this is how a renamed reading of an entry
/// is compared with another reading of it.
pub(super) fn same_keys(left: &[Selection], right: &[Selection]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter().zip(right).all(|(left, right)| {
        match (left, right) {
            (Selection::Field(left), Selection::Field(right)) => {
                left.response_name() == right.response_name()
                    && same_keys(
                        &left.selection_set.selections,
                        &right.selection_set.selections,
                    )
            }
            (Selection::InlineFragment(left), Selection::InlineFragment(right)) => same_keys(
                &left.selection_set.selections,
                &right.selection_set.selections,
            ),
            // Spreads are inlined before a fetch's selections reach here.
            _ => false,
        }
    })
}

//==================================================================================================
// What a subgraph demands
//==================================================================================================

/// Everything a subgraph demands before it will resolve an object of type `object_type` as an
/// entity of `entity_type` for one fetch through one `@key`: `__typename`, the key's fields, and
/// the `@requires` field sets of the fields the fetch selects on it, written at `object_type`.
///
/// `requires` is [`requires_of_selections`], which only the key does not vary, computed once by
/// the caller. `Err` means the key does not typecheck at `object_type`: that key cannot be the one
/// the subgraph resolves the object by.
pub(super) fn entity_fetch_requirement(
    supergraph_schema: &ValidFederationSchema,
    object_type: &Name,
    key_fields: &str,
    requires: &[Selection],
) -> Result<Vec<Selection>, ComparisonError> {
    let mut selections = super::subgraph::parse_field_set(
        supergraph_schema,
        object_type,
        &format!("__typename {key_fields}"),
    )?;
    selections.extend(requires.iter().cloned());
    Ok(vec![inline_fragment(object_type.clone(), selections)])
}

/// The `@requires` field sets of the fields a fetch selects on an entity, each kept under the
/// directives of the selection that carries it, as `collect_require_condition` inherits each
/// variant's Boolean clause.
///
/// Looked up in the subgraph at `entity_type`, the type as the subgraph declares it, and parsed
/// against the supergraph at `object_type`, where they are demanded.
pub(super) fn requires_of_selections(
    supergraph_schema: &ValidFederationSchema,
    subgraph: &Subgraph<'_>,
    entity_type: &Name,
    object_type: &Name,
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
                    super::subgraph::parse_field_set(supergraph_schema, object_type, &field_set)?,
                )
            }
            Selection::InlineFragment(fragment) => (
                &fragment.directives,
                requires_of_selections(
                    supergraph_schema,
                    subgraph,
                    entity_type,
                    object_type,
                    &fragment.selection_set.selections,
                )?,
            ),
            // Spreads are inlined before a fetch's selections reach here.
            Selection::FragmentSpread(_) => continue,
        };
        out.extend(under_directives(directives, object_type, required));
    }
    Ok(out)
}

//==================================================================================================
// Conflicting demands between two `@requires`

/// Reports two demands on one response key that are different resolver calls — FED-504.
///
/// [`requires_of_selections`] concatenates the `@requires` field sets of every field a fetch
/// selects on the entity; it does not merge them, because nothing here can merge two calls that
/// disagree. When they do disagree the fetch can only make one of them, and one of the two fields
/// is fed data it did not ask for.
/// - Nothing else in this checker can see it. The plan's `requires` entry has had its arguments
///   dropped by `trim_requires_selection_set`, and the concatenation is handed to `query_compare`
///   as one document, where the field-merging rule the model assumes makes the first occurrence
///   stand for the rest.
/// - Two demands under conditions that cannot both hold are not in conflict, so the comparison is
///   made only where the positions overlap.
/// - [`CheckerOptions::check_requires_conflict`] can turn it off: the planner emits these plans
///   today.
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
        let key = demand.field.response_name();
        if !keys.contains(&key) {
            keys.push(key);
        }
    }

    for key in keys {
        let here: Vec<&Demand<'_>> = demands
            .iter()
            .filter(|demand| demand.field.response_name() == key)
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

pub(super) fn inline_fragment(type_condition: Name, selections: Vec<Selection>) -> Selection {
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

    //==============================================================================================
    // Conflicting demands between two `@requires`

    /// Two `@requires` field sets, concatenated the way `requires_of_selections` concatenates them.
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
}
