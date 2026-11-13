//! Output renames filtered by type.
//!
//! Ports of the model's `OutputRenameByType` tests. A `Parent` is an `A` or a `B`, each with a
//! `child`; `source` selects every parent's child once, as `c`, and an output rewrite renames it to
//! `child` where the parent is an `A`. The router tests the filter with `is_object_of_type`, which
//! an object without `__typename` passes, so what a `B` ends up with depends on whether the fetch
//! selected its `__typename`. Each plan is written inline as JSON.

use serde_json::Value;
use serde_json::json;

use super::CorrectnessError;
use super::Fixture;

const INCOMPLETE: &str = "query plan does not fetch everything the operation requests";

fn correlated_children() -> Fixture {
    Fixture::new(include_str!("../testdata/correlated_children.graphql"))
}

/// A root fetch to `source` selecting `operation`, renaming as `path` says.
fn renaming_fetch(operation: &str, path: Value, rename_key_to: &str) -> Value {
    json!({ "Fetch": {
        "subgraph_name": "source",
        "variable_usages": [],
        "operation_document": operation,
        "operation_kind": "query",
        "input_rewrites": [],
        "output_rewrites": [{ "KeyRenamer": { "path": path, "rename_key_to": rename_key_to } }],
        "context_rewrites": []
    } })
}

/// `parent.c` renamed to `child` where the parent is an `A`.
fn c_to_child_under_a() -> Value {
    json!([{ "Key": ["parent", null] }, { "TypenameEquals": "A" }, { "Key": ["c", null] }])
}

const CHILD_VALUES: &str = "c: child { ... on X { value } ... on Y { value } }";

/// The plan, its fetch selecting the parent's `__typename` when `observed`.
fn renamed(observed: bool) -> Value {
    let typename = if observed { "__typename " } else { "" };
    renaming_fetch(
        &format!("{{ parent {{ {typename}{CHILD_VALUES} }} }}"),
        c_to_child_under_a(),
        "child",
    )
}

/// The plan with `parent` selected twice, `c` in one selection and `__typename` in the other. The
/// router merges the two, so every parent has `__typename`.
fn split() -> Value {
    renaming_fetch(
        &format!("{{ parent {{ {CHILD_VALUES} }} parent {{ __typename }} }}"),
        c_to_child_under_a(),
        "child",
    )
}

/// An `A`'s child read under `child`, and a `B`'s under `c`.
const EACH_TYPE_UNDER_ITS_KEY: &str = "{ parent { ... on A { child { ... on X { value } } } \
                                        ... on B { c: child { ... on Y { value } } } } }";
const A_UNDER_CHILD: &str = "{ parent { ... on A { child { ... on X { value } } } } }";
const B_UNDER_CHILD: &str = "{ parent { ... on B { child { ... on Y { value } } } } }";
const EVERY_PARENT_UNDER_CHILD: &str =
    "{ parent { child { ... on X { value } ... on Y { value } } } }";

fn check(plan: Value, operation: &str) -> Result<(), CorrectnessError> {
    let fixture = correlated_children();
    fixture.check(&fixture.written_plan(&plan.to_string()), operation)
}

#[track_caller]
fn expect_incomplete(result: Result<(), CorrectnessError>) {
    let error = result
        .expect_err("expected the check to reject this plan")
        .to_string();
    assert!(error.contains(INCOMPLETE), "{error}");
}

// The model's `renameUnderARuntimeTypeServesEachTypeUnderItsKey`.
#[test]
fn a_rename_under_a_runtime_type_serves_each_type_under_its_key() {
    check(renamed(true), EACH_TYPE_UNDER_ITS_KEY).unwrap();
}

// A `B`'s child stays under `c`, so a client reading it under `child` is not served. The model's
// `renameUnderARuntimeTypeLeavesTheOtherType`; accepted before.
#[test]
fn a_rename_under_a_runtime_type_leaves_the_other_type() {
    expect_incomplete(check(renamed(true), B_UNDER_CHILD));
}

// The model's `renameUnderARuntimeTypeDoesNotServeEveryParent`; accepted before.
#[test]
fn a_rename_under_a_runtime_type_does_not_serve_every_parent() {
    expect_incomplete(check(renamed(true), EVERY_PARENT_UNDER_CHILD));
}

// Without the parent's `__typename`, `is_object_of_type` passes every parent, so every child is
// renamed. The model's `renameWithoutTypenameMovesEveryParentsField`.
#[test]
fn a_rename_without_typename_moves_every_parents_field() {
    check(renamed(false), EVERY_PARENT_UNDER_CHILD).unwrap();
}

// Upstream's `a_rename_under_a_runtime_type_moves_that_types_field_alone` accepts this plan; the
// router renames a `B`'s child too, leaving nothing under `c`. The model's
// `renameWithoutTypenameLeavesNothingUnderTheOldKey`.
#[test]
fn a_rename_without_typename_leaves_nothing_under_the_old_key() {
    expect_incomplete(check(renamed(false), EACH_TYPE_UNDER_ITS_KEY));
}

// `__typename` from the other selection reaches every parent, so a `B` keeps `c`. The model's
// `renameWithTypenameInAnotherSelectionLeavesTheOtherType`.
#[test]
fn a_rename_with_typename_in_another_selection_leaves_the_other_type() {
    expect_incomplete(check(split(), B_UNDER_CHILD));
}

// The model's `renameWithTypenameInAnotherSelectionMovesTheFilteredType`.
#[test]
fn a_rename_with_typename_in_another_selection_moves_the_filtered_type() {
    check(split(), A_UNDER_CHILD).unwrap();
}

// `__typename` in another selection at `parent` is read as possibly reaching a parent, so a `B`'s
// child is credited under neither key. A known conservative point: the router serves it under `c`.
// The model's `renameWithTypenameInAnotherSelectionCreditsNeitherKey`.
#[test]
fn a_rename_with_typename_in_another_selection_credits_neither_key() {
    expect_incomplete(check(split(), EACH_TYPE_UNDER_ITS_KEY));
}

/// The plan renaming `w` back to `value` below the child, where the parent is an `A`.
fn renamed_below() -> Value {
    renaming_fetch(
        "{ parent { __typename c: child { ... on X { w: value } ... on Y { w: value } } } }",
        json!([
            { "Key": ["parent", null] },
            { "TypenameEquals": "A" },
            { "Key": ["c", null] },
            { "Key": ["w", null] }
        ]),
        "value",
    )
}

// A `TypenameEquals` before a key the path descends past filters the descent too: the renamer
// reaches `w` below an `A`'s child, not a `B`'s, whose `__typename` the fetch selected. The model's
// `typenameEqualsFiltersTheDescent`.
#[test]
fn a_typename_filter_restricts_the_descent() {
    check(
        renamed_below(),
        "{ parent { ... on A { c: child { ... on X { value } } } \
         ... on B { c: child { ... on Y { w: value } } } } }",
    )
    .unwrap();
    expect_incomplete(check(
        renamed_below(),
        "{ parent { ... on B { c: child { ... on Y { value } } } } }",
    ));
}
