//! What a deferred block reads from the fetches it names.
//!
//! The router runs a deferred block on what its `defer` node was given, merged with the response
//! of each fetch the block `depends` on: a fetch sends what it added and the fields leading to it,
//! where it ran. These cases pair a block's read with the fetches it names, against a control
//! without the `defer`: a value fetched for one parent type, a field written under one condition,
//! a contextual value off a sibling branch, and a dependency run under a guard. Each plan is
//! written inline as JSON.

use serde_json::Value;
use serde_json::json;

use super::CorrectnessError;
use super::Fixture;

//==================================================================================================
// Writing plans

/// A fetch to `subgraph` with no `requires`, selecting `operation`.
fn fetch(subgraph: &str, operation: &str) -> Value {
    json!({ "Fetch": {
        "subgraph_name": subgraph,
        "variable_usages": [],
        "operation_document": operation,
        "operation_kind": "query",
        "input_rewrites": [],
        "output_rewrites": [],
        "context_rewrites": []
    } })
}

/// `node` with the fetch id `id`, which a deferred block names in its `depends`.
fn with_id(mut node: Value, id: u64) -> Value {
    node["Fetch"]["id"] = json!(id);
    node
}

/// The fields of a `requires` entry, each a name.
fn fields(names: &[&str]) -> Vec<Value> {
    names
        .iter()
        .map(|name| json!({ "kind": "Field", "name": name }))
        .collect()
}

/// A `requires` entry on `type_condition`.
fn entry(type_condition: &str, selections: Vec<Value>) -> Value {
    json!({ "kind": "InlineFragment", "typeCondition": type_condition, "selections": selections })
}

/// An entity fetch to `subgraph`, sent `entries` and selecting `cases` under `_entities`.
fn entity_fetch(subgraph: &str, entries: Vec<Value>, cases: &str) -> Value {
    json!({ "Fetch": {
        "subgraph_name": subgraph,
        "variable_usages": [],
        "requires": entries,
        "operation_document": format!(
            "query($representations: [_Any!]!) \
             {{ _entities(representations: $representations) {{ {cases} }} }}"
        ),
        "operation_kind": "query",
        "input_rewrites": [],
        "output_rewrites": [],
        "context_rewrites": []
    } })
}

/// A flatten node at `path`, `.`-separated keys with `|[A]` for a condition.
fn flatten(path: &str, node: Value) -> Value {
    let path: Vec<Value> = path
        .split('.')
        .map(|element| match element.split_once("|[") {
            Some((key, condition)) => json!({ "Key": [key, [condition.trim_end_matches(']')]] }),
            None => json!({ "Key": [element, null] }),
        })
        .collect();
    json!({ "Flatten": { "path": path, "node": node } })
}

fn sequence(nodes: Vec<Value>) -> Value {
    json!({ "Sequence": { "nodes": nodes } })
}

/// A condition node on `$variable`.
fn condition(variable: &str, if_clause: Option<Value>, else_clause: Option<Value>) -> Value {
    json!({ "Condition": {
        "condition_variable": variable,
        "if_clause": if_clause,
        "else_clause": else_clause
    } })
}

/// A `defer` node: `primary`, then one deferred block naming the fetch ids `depends`.
fn defer(primary: Value, depends: &[u64], deferred: Value) -> Value {
    let depends: Vec<Value> = depends
        .iter()
        .map(|id| json!({ "id": id.to_string() }))
        .collect();
    json!({ "Defer": {
        "primary": { "sub_selection": null, "node": primary },
        "deferred": [{
            "depends": depends,
            "label": null,
            "query_path": [],
            "sub_selection": null,
            "node": deferred
        }]
    } })
}

fn check(fixture: &Fixture, plan: Value, operation: &str) -> Result<(), CorrectnessError> {
    fixture.check(&fixture.written_plan(&plan.to_string()), operation)
}

/// Expects a rejection for `reason`, naming `detail`.
#[track_caller]
fn expect_rejected(result: Result<(), CorrectnessError>, reason: &str, detail: &str) {
    let error = result
        .expect_err("expected the check to reject this plan")
        .to_string();
    assert!(error.contains(reason) && error.contains(detail), "{error}");
}

const UNFETCHED_KEY: &str = "reads a key the plan has not fetched";
const UNSENT_DEMAND: &str = "the plan has not fetched what the subgraph demands";

//==================================================================================================
// A value fetched for one parent type

/// In `source` an `A`'s `child` is an `X` and a `B`'s a `Y`; `consumer` resolves either's `score`
/// from its `value`.
fn correlated_scores() -> Fixture {
    Fixture::new(include_str!("../testdata/correlated_scores.graphql"))
}

const PARENT_CHILD_SCORES: &str = "{ parent { child { ... on X { score } ... on Y { score } } } }";

/// Each `parent.child`'s `score` from `consumer`, sent its `value` at either type.
fn scores() -> Value {
    let key_and_value = || fields(&["__typename", "id", "value"]);
    flatten(
        "parent.child",
        entity_fetch(
            "consumer",
            vec![entry("X", key_and_value()), entry("Y", key_and_value())],
            "... on X { score } ... on Y { score }",
        ),
    )
}

/// The children's keys, and their `value` under the `A` and under the `B` when `b_value`.
fn check_scores(b_value: bool) -> Result<(), CorrectnessError> {
    let value = if b_value { " value" } else { "" };
    let plan = sequence(vec![
        fetch(
            "source",
            &format!(
                "{{ parent {{ ... on A {{ child {{ __typename id value }} }} \
                 ... on B {{ child {{ __typename id{value} }} }} }} }}"
            ),
        ),
        scores(),
    ]);
    check(&correlated_scores(), plan, PARENT_CHILD_SCORES)
}

// Upstream's `a_value_fetched_under_one_parent_type_does_not_serve_the_other`.
#[test]
fn value_fetched_under_each_parent_type_is_accepted() {
    check_scores(true).unwrap();
}

// The `X`'s `value`, fetched under the `A`, does not serve the `Y` under the `B`. Upstream's
// `a_value_fetched_under_one_parent_type_does_not_serve_the_other`.
#[test]
fn value_fetched_under_one_parent_type_is_rejected() {
    expect_rejected(
        check_scores(false),
        UNSENT_DEMAND,
        "does not select `value` on Y",
    );
}

/// The same, the scores deferred: the keys (fetch 0), the `A`'s child's `value` (fetch 1) and the
/// `B`'s (fetch 2) in the primary block, and the block naming `depends`.
fn check_deferred_scores(depends: &[u64]) -> Result<(), CorrectnessError> {
    let primary = sequence(vec![
        with_id(
            fetch(
                "source",
                "{ parent { ... on A { child { __typename id } } \
                 ... on B { child { __typename id } } } }",
            ),
            0,
        ),
        with_id(
            fetch("source", "{ parent { ... on A { child { value } } } }"),
            1,
        ),
        with_id(
            fetch("source", "{ parent { ... on B { child { value } } } }"),
            2,
        ),
    ]);
    check(
        &correlated_scores(),
        defer(primary, depends, scores()),
        PARENT_CHILD_SCORES,
    )
}

// Upstream's `a_deferred_block_reads_each_parent_types_value_from_the_fetch_sending_it`.
#[test]
fn deferred_block_naming_each_types_value_fetch_is_accepted() {
    check_deferred_scores(&[0, 1, 2]).unwrap();
}

// Without fetch 2 the block is sent no `value` for a `Y`, though the child's runtime types are what
// the whole primary block makes them. Upstream's
// `a_deferred_block_reads_each_parent_types_value_from_the_fetch_sending_it`.
#[test]
fn deferred_block_missing_one_types_value_fetch_is_rejected() {
    expect_rejected(
        check_deferred_scores(&[0, 1]),
        UNSENT_DEMAND,
        "does not select `value` on Y",
    );
}

//==================================================================================================
// A field written under one condition

/// `consumer`'s `score` on `entity`, sent its key and its `child`'s `id` where the child is a `Y`.
fn score_where_the_child_is_a_y(entity: &str) -> Value {
    let mut selections = fields(&["__typename", "id"]);
    selections.push(json!({
        "kind": "Field",
        "name": "child",
        "selections": [entry("Y", fields(&["id"]))]
    }));
    entity_fetch(
        "consumer",
        vec![entry(entity, selections)],
        &format!("... on {entity} {{ score }}"),
    )
}

/// The parent's keys (fetch 0); its `child` under `$b`, an `X` (fetch 1), and otherwise its `y`
/// aliased to `child`, a `Y` (fetch 2); under `¬$b` each child's value from `values` (fetch 3);
/// and a block naming `depends` giving the parent its `score` under `$b`.
fn check_aliased_children(depends: &[u64]) -> Result<(), CorrectnessError> {
    let key = || fields(&["__typename", "id"]);
    let values = entity_fetch(
        "values",
        vec![entry("X", key()), entry("Y", key())],
        "... on X { xval } ... on Y { yval }",
    );
    let primary = sequence(vec![
        with_id(fetch("source", "{ parent { __typename id } }"), 0),
        condition(
            "b",
            Some(with_id(
                fetch("source", "{ parent { child { __typename id } } }"),
                1,
            )),
            Some(with_id(
                fetch("source", "{ parent { child: y { __typename id } } }"),
                2,
            )),
        ),
        condition("b", None, Some(flatten("parent.child", with_id(values, 3)))),
    ]);
    let deferred = condition(
        "b",
        Some(flatten("parent", score_where_the_child_is_a_y("Parent"))),
        None,
    );
    check(
        &Fixture::new(include_str!("../testdata/aliased_children.graphql")),
        defer(primary, depends, deferred),
        "query($b: Boolean!) { parent { score @include(if: $b) } }",
    )
}

// Upstream's `a_deferred_dependency_sends_no_field_its_condition_excludes`.
#[test]
fn deferred_block_naming_the_fetch_writing_the_child_is_accepted() {
    check_aliased_children(&[0, 1, 3]).unwrap();
}

// Fetch 3 runs under `¬$b`, at the `Y` fetch 2 wrote, so what it sends leads through fetch 2's
// aliased `y`, not fetch 1's `child`, which the `score` under `$b` reads. Upstream's
// `a_deferred_dependency_sends_no_field_its_condition_excludes`.
#[test]
fn deferred_dependency_run_under_the_other_condition_sends_no_child_is_rejected() {
    expect_rejected(
        check_aliased_children(&[0, 3]),
        UNFETCHED_KEY,
        "in `parent`: `child` is not read on Parent",
    );
}

/// The holder's `a` aliased to `b`, an `A`, under `$b` (fetch 0), and otherwise its `b` (fetch 1);
/// each `B`'s `child` there (fetch 2); under `$b` each child's `xval` (fetch 3); and a block naming
/// `depends` giving the `B` its `score` under `¬$b`.
fn check_aliased_parents(depends: &[u64]) -> Result<(), CorrectnessError> {
    let key = || fields(&["__typename", "id"]);
    let primary = sequence(vec![
        condition(
            "b",
            Some(with_id(
                fetch("source", "{ holder { b: a { __typename id } } }"),
                0,
            )),
            Some(with_id(
                fetch("source", "{ holder { b { __typename id } } }"),
                1,
            )),
        ),
        flatten(
            "holder.b",
            with_id(
                entity_fetch(
                    "source",
                    vec![entry("B", key())],
                    "... on B { child { __typename id } }",
                ),
                2,
            ),
        ),
        condition(
            "b",
            Some(flatten(
                "holder.b.child",
                with_id(
                    entity_fetch("values", vec![entry("X", key())], "... on X { xval }"),
                    3,
                ),
            )),
            None,
        ),
    ]);
    let deferred = condition(
        "b",
        None,
        Some(flatten("holder.b", score_where_the_child_is_a_y("B"))),
    );
    check(
        &Fixture::new(include_str!("../testdata/aliased_parents.graphql")),
        defer(primary, depends, deferred),
        "query($b: Boolean!) { holder { b @skip(if: $b) { score } } }",
    )
}

// Upstream's `a_deferred_dependency_sends_no_field_below_a_parent_its_condition_excludes`.
#[test]
fn deferred_block_naming_the_fetch_writing_the_childs_parent_is_accepted() {
    check_aliased_parents(&[1, 2, 3]).unwrap();
}

// Fetch 3 runs under `$b`, where the holder's `b` is the `A` fetch 0 wrote, which has no `child`,
// so it adds nothing and sends no field leading anywhere: not fetch 2's `child`, whose parent is a
// `B` only under `¬$b`. Upstream's
// `a_deferred_dependency_sends_no_field_below_a_parent_its_condition_excludes`.
#[test]
fn deferred_dependency_adding_nothing_sends_no_child_is_rejected() {
    expect_rejected(
        check_aliased_parents(&[1, 3]),
        UNFETCHED_KEY,
        "in `holder`: in `b`: `child` is not read on B",
    );
}

//==================================================================================================
// A contextual value off a sibling branch

/// A setter's `left` and `right` are each an `A` or a `B`, independently; `consumer` resolves an
/// `A`'s `info`, taking `c` from the setter's `right`'s `val`.
fn divergent_context() -> Fixture {
    Fixture::new(include_str!("../testdata/divergent_context.graphql"))
}

const SETTER_LEFT_INFO: &str = "{ setter { left { ... on A { info } } } }";
const CONTEXT_NOT_FETCHED: &str =
    "the contextual value for `$contextualArgument_1_0` is not always fetched where the fetch runs";

/// `consumer` asked for each `A`'s `info` at `setter.left|[A]`, its context read off the setter's
/// `right`.
fn info_from_right() -> Value {
    flatten(
        "setter.left|[A]",
        json!({ "Fetch": {
            "subgraph_name": "consumer",
            "variable_usages": ["contextualArgument_1_0"],
            "requires": [entry("A", fields(&["__typename", "id"]))],
            "operation_document": "query($representations: [_Any!]!, $contextualArgument_1_0: \
                String) { _entities(representations: $representations) { ... on A { \
                info(c: $contextualArgument_1_0) } } }",
            "operation_kind": "query",
            "input_rewrites": [],
            "output_rewrites": [],
            "context_rewrites": [{ "KeyRenamer": {
                "path": [
                    "Parent",
                    { "TypenameEquals": "Setter" },
                    { "Key": ["right", null] },
                    { "Key": ["val", null] }
                ],
                "rename_key_to": "contextualArgument_1_0"
            } }]
        } }),
    )
}

/// The setter's `left` and `right`, the `right` with `right_selections`.
fn setter_fetch(right_selections: &str) -> Value {
    fetch(
        "base",
        &format!(
            "{{ setter {{ left {{ __typename ... on A {{ id }} }} \
             right {{ __typename {right_selections} }} }} }}"
        ),
    )
}

// The `right`'s `val`, fetched for either type, is always there. Upstream's
// `a_context_on_a_sibling_branch_is_owed_whatever_that_branch_is`.
#[test]
fn context_off_a_sibling_branch_fetched_for_every_type_is_accepted() {
    let plan = sequence(vec![setter_fetch("val"), info_from_right()]);
    check(&divergent_context(), plan, SETTER_LEFT_INFO).unwrap();
}

// Fetched under the `right`'s `A` alone, it is missing where the `right` is a `B`, whatever the
// `left` is. Upstream's `a_context_on_a_sibling_branch_is_owed_whatever_that_branch_is`.
#[test]
fn context_off_a_sibling_branch_fetched_for_one_type_is_rejected() {
    let plan = sequence(vec![setter_fetch("... on A { val }"), info_from_right()]);
    expect_rejected(
        check(&divergent_context(), plan, SETTER_LEFT_INFO),
        CONTEXT_NOT_FETCHED,
        "fetch runs at: setter.left|[A]",
    );
}

/// The same, deferred: the setter (fetch 0), the `right`'s `val` for an `A` (fetch 1) and for a
/// `B` (fetch 2), and the block naming `depends`.
fn check_deferred_context(depends: &[u64]) -> Result<(), CorrectnessError> {
    let primary = sequence(vec![
        with_id(setter_fetch(""), 0),
        with_id(
            fetch("base", "{ setter { right { ... on A { val } } } }"),
            1,
        ),
        with_id(
            fetch("base", "{ setter { right { ... on B { val } } } }"),
            2,
        ),
    ]);
    check(
        &divergent_context(),
        defer(primary, depends, info_from_right()),
        SETTER_LEFT_INFO,
    )
}

// Upstream's `a_deferred_context_on_a_sibling_branch_is_read_from_the_fetches_named`.
#[test]
fn deferred_context_named_for_every_type_is_accepted() {
    check_deferred_context(&[0, 1, 2]).unwrap();
}

// Upstream's `a_deferred_context_on_a_sibling_branch_is_read_from_the_fetches_named`.
#[test]
fn deferred_context_named_for_one_type_is_rejected() {
    expect_rejected(
        check_deferred_context(&[0, 1]),
        CONTEXT_NOT_FETCHED,
        "fetch runs at: setter.left|[A]",
    );
}

//==================================================================================================
// A dependency run under a guard

/// The parent's child (fetch 0), then under `$v` its key and `seed` again (fetch 1), then a block
/// naming fetch 1 giving each child its `result` from `consumer`, at `path`, under `$v` when
/// `guarded`.
fn check_deferred_results(path: &str, guarded: bool) -> Result<(), CorrectnessError> {
    let results = flatten(
        path,
        entity_fetch(
            "consumer",
            vec![entry("Child", fields(&["__typename", "id", "seed"]))],
            "... on Child { result }",
        ),
    );
    let results = if guarded {
        condition("v", Some(results), None)
    } else {
        results
    };
    let primary = sequence(vec![
        with_id(
            fetch("first", "{ parent { __typename child { __typename id } } }"),
            0,
        ),
        condition(
            "v",
            Some(with_id(
                fetch("first", "{ parent { child { __typename id seed } } }"),
                1,
            )),
            None,
        ),
    ]);
    check(
        &Fixture::new(include_str!("../testdata/deferred_seed.graphql")),
        defer(primary, &[1], results),
        "query($v: Boolean!) { parent { child { result @include(if: $v) } } }",
    )
}

// Fetch 1 sends the child's `seed` under `$v`, where the block's consumer runs. Upstream's
// `a_deferred_block_reads_a_guarded_dependency_under_its_guard`.
#[test]
fn deferred_block_reading_a_guarded_dependency_under_its_guard_is_accepted() {
    check_deferred_results("parent.child", true).unwrap();
}

// Run unconditionally, the consumer is sent nothing under `¬$v`. Upstream's
// `a_deferred_block_reads_a_guarded_dependency_under_its_guard`.
#[test]
fn deferred_block_reading_a_guarded_dependency_unguarded_is_rejected() {
    expect_rejected(
        check_deferred_results("parent.child", false),
        UNFETCHED_KEY,
        "`parent` is not read on Query",
    );
}

// A path condition on the parent reads its `__typename`, which only fetch 0, not named, wrote.
// Upstream's `a_deferred_block_reads_a_guarded_dependency_under_its_guard`.
#[test]
fn deferred_block_path_condition_reading_an_unnamed_fetchs_typename_is_rejected() {
    expect_rejected(
        check_deferred_results("parent|[Parent].child", true),
        "assuming: $v",
        "does not select `__typename` on Parent",
    );
}
