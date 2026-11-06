//! The input rewrites' structure, and entries under literal and nested conditions.
//!
//! Ports of the model's `ConditionalRequires` tests on these. Most check a two-fetch plan: `one`
//! fetches what is asked of `t`, then `two` is sent the `requires` entries and runs the `_entities`
//! fragments. The input-rewrite tests call the three structural checks directly, as the model
//! does, on an entry that copies `u` whole.

use apollo_compiler::executable::Selection;
use apollo_compiler::name;

use super::super::super::CorrectnessError;
use super::super::representation::entry_configurations;
use super::super::representation::input_rewrites_idempotent;
use super::super::representation::input_rewrites_separated;
use super::super::representation::input_rewrites_well_formed;
use super::super::requires::entity_fetch_requirement;
use super::super::requires::requires_of_selections;
use super::super::subgraph::Subgraph;
use super::Fixture;
use crate::query_plan::FetchNode;
use crate::query_plan::QueryPlan;
use crate::query_plan::requires_selection;

/// `one` serves `t: T` with its key, `a`, `e` and `u { left right }`; `two` resolves a `T` by `id`,
/// and its `b`, `d` and `both` require `a`, `e` and `u { left right }`; `projection` resolves a `T`
/// by `id` too, its `b` and `d` requiring `u { right }` and `u { left }`.
fn fixture() -> Fixture {
    Fixture::new(include_str!("../testdata/conditional_entries.graphql"))
}

//==================================================================================================
// Writing plans

/// The variables `source` reads, in order, `$representations` aside.
fn variables(source: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for (index, _) in source.match_indices('$') {
        let name: String = source[index + 1..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name != "representations" && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// `body` as a query declaring every variable it reads a `Boolean!`, and the list of them.
fn query(prefix: &[&str], body: &str) -> (String, String) {
    let names = variables(body);
    let mut declarations: Vec<String> = prefix.iter().map(|d| d.to_string()).collect();
    declarations.extend(names.iter().map(|name| format!("${name}: Boolean!")));
    let usages = names
        .iter()
        .map(|name| format!(r#""{name}""#))
        .collect::<Vec<_>>()
        .join(", ");
    if declarations.is_empty() {
        (body.to_string(), format!("[{usages}]"))
    } else {
        (
            format!("query({}) {body}", declarations.join(", ")),
            format!("[{usages}]"),
        )
    }
}

/// The client operation asking for `selected` under `t`.
fn operation(selected: &str) -> String {
    query(&[], &format!("{{ t {{ {selected} }} }}")).0
}

fn input(name: &str) -> String {
    format!(r#"{{ "kind": "Field", "name": "{name}" }}"#)
}

fn input_below(name: &str, children: &[String]) -> String {
    format!(
        r#"{{ "kind": "Field", "name": "{name}", "selections": [{}] }}"#,
        children.join(", ")
    )
}

/// An inline fragment of a `requires` entry, on `type_condition` and under `conditions`.
fn fragment(type_condition: Option<&str>, conditions: &[String], children: &[String]) -> String {
    let type_condition = type_condition
        .map(|name| format!(r#""typeCondition": "{name}", "#))
        .unwrap_or_default();
    format!(
        r#"{{ "kind": "InlineFragment", {type_condition}"conditions": [{}], "selections": [{}] }}"#,
        conditions.join(", "),
        children.join(", ")
    )
}

/// An entry on `T`.
fn entry(children: &[String]) -> String {
    fragment(Some("T"), &[], children)
}

fn include(variable: &str) -> String {
    format!(r#"{{ "kind": "include", "if": "{variable}" }}"#)
}

fn skip(variable: &str) -> String {
    format!(r#"{{ "kind": "skip", "if": "{variable}" }}"#)
}

fn include_literal(value: bool) -> String {
    format!(r#"{{ "kind": "include", "if": {value} }}"#)
}

fn skip_literal(value: bool) -> String {
    format!(r#"{{ "kind": "skip", "if": {value} }}"#)
}

fn key_inputs() -> Vec<String> {
    vec![input("__typename"), input("id")]
}

fn with_key(inputs: &[String]) -> Vec<String> {
    let mut out = key_inputs();
    out.extend(inputs.iter().cloned());
    out
}

/// `one` fetches `fetched` under `t`, then `two` is sent `entries` and runs `cases` under
/// `_entities`.
fn plan(fixture: &Fixture, fetched: &str, entries: &[String], cases: &str) -> QueryPlan {
    let (one_operation, one_usages) = query(&[], &format!("{{ t {{ {fetched} }} }}"));
    let (two_operation, two_usages) = query(
        &["$representations: [_Any!]!"],
        &format!("{{ _entities(representations: $representations) {{ {cases} }} }}"),
    );
    let entries = entries.join(", ");
    fixture.written_plan(&format!(
        r#"{{ "Sequence": {{ "nodes": [
            {{ "Fetch": {{
                "subgraph_name": "one",
                "variable_usages": {one_usages},
                "operation_document": "{one_operation}",
                "operation_kind": "query",
                "input_rewrites": [],
                "output_rewrites": [],
                "context_rewrites": []
            }} }},
            {{ "Flatten": {{
                "path": [{{ "Key": ["t", null] }}],
                "node": {{ "Fetch": {{
                    "subgraph_name": "two",
                    "variable_usages": {two_usages},
                    "requires": [{entries}],
                    "operation_document": "{two_operation}",
                    "operation_kind": "query",
                    "input_rewrites": [],
                    "output_rewrites": [],
                    "context_rewrites": []
                }} }}
            }} }}
        ] }} }}"#
    ))
}

/// The plan above for a client asking for `selected` under `t`, its cases one `T` fragment
/// selecting the same unless given.
fn checked(
    fetched: &str,
    entries: &[String],
    selected: &str,
    cases: Option<&str>,
) -> Result<(), CorrectnessError> {
    let fixture = fixture();
    let cases = cases.map_or_else(|| format!("... on T {{ {selected} }}"), str::to_string);
    let plan = plan(&fixture, fetched, entries, &cases);
    fixture.check(&plan, &operation(selected))
}

#[track_caller]
fn assert_rejected<E: std::fmt::Display>(result: Result<(), E>, reason: &str) {
    match result {
        Ok(()) => panic!("expected the check to reject this plan"),
        Err(error) => {
            let error = error.to_string();
            assert!(error.contains(reason), "{error}");
        }
    }
}

//==================================================================================================
// The input rewrites' structure

fn key(name: &str) -> String {
    format!(r#"{{ "Key": ["{name}", null] }}"#)
}

fn renamer(path: &[String], to: &str) -> String {
    format!(
        r#"{{ "KeyRenamer": {{ "path": [{}], "rename_key_to": "{to}" }} }}"#,
        path.join(", ")
    )
}

/// Whether `rewrites` are well formed and separated, and whether they are idempotent on the demand
/// of `b` and `d` together, `u { right }` and `u { left }`, through each `@key` of `T` in
/// `projection`, for a fetch whose one entry copies `u` whole.
fn copied_rewrites_checked(rewrites: &[String]) -> (Result<(), String>, bool) {
    let fixture = fixture();
    let schema = &fixture.supergraph_schema;
    let fetch: FetchNode = serde_json::from_str(&format!(
        r#"{{
            "subgraph_name": "projection",
            "id": null,
            "variable_usages": [],
            "requires": [{}],
            "operation_document": "",
            "operation_name": null,
            "operation_kind": "query",
            "input_rewrites": [{}],
            "output_rewrites": [],
            "context_rewrites": []
        }}"#,
        entry(&with_key(&[input("u")])),
        rewrites.join(", ")
    ))
    .expect("valid fetch json");
    let structure =
        input_rewrites_well_formed(&fetch).and_then(|()| input_rewrites_separated(schema, &fetch));

    let document = fixture.operation("{ t { b d } }");
    let Selection::Field(t) = &document
        .operations
        .get(None)
        .unwrap()
        .selection_set
        .selections[0]
    else {
        unreachable!("one root field")
    };
    let entity = name!("T");
    let subgraph = Subgraph::new(&fixture.subgraphs["projection"]).expect("a federation subgraph");
    let requires = requires_of_selections(
        schema,
        &subgraph,
        &entity,
        &entity,
        &t.selection_set.selections,
    )
    .expect("well-formed @requires");
    let idempotent = subgraph.keys(&entity).unwrap().iter().all(|key| {
        let demand = entity_fetch_requirement(schema, &entity, &key.fields, &requires)
            .expect("the key applies to `T`");
        input_rewrites_idempotent(schema, &fetch, &demand)
    });
    (structure, idempotent)
}

#[track_caller]
fn assert_structure(checked: (Result<(), String>, bool), reason: &str, idempotent: bool) {
    match checked.0 {
        Ok(()) => panic!("expected the input rewrites to be rejected"),
        Err(error) => assert!(error.contains(reason), "{error}"),
    }
    assert_eq!(checked.1, idempotent, "idempotence");
}

// The router names a copied `r` back to `right` below `u`. The copy might hold `right` already,
// but a key read under its own name only counts where an entry lists it, so the two are not sent
// under one key; and the rename leaves the demanded `right` and `left` alone.
#[test]
fn alias_named_back_is_separated_and_idempotent() {
    let (structure, idempotent) =
        copied_rewrites_checked(&[renamer(&[key("u"), key("r")], "right")]);
    structure.unwrap();
    assert!(idempotent);
}

// Nothing shows a copied `u` never holds both `right` and `left`, so renaming both to `x` may send
// two values under one key. It also renames the demanded keys away.
#[test]
fn renamers_onto_one_key_are_not_separated() {
    assert_structure(
        copied_rewrites_checked(&[
            renamer(&[key("u"), key("right")], "x"),
            renamer(&[key("u"), key("left")], "x"),
        ]),
        "under one key",
        false,
    );
}

// One key renamed to two, as reading `{ c: a, c: b }` would need: the first renamer leaves no `c`
// for the second.
#[test]
fn renamers_from_one_key_are_ill_formed() {
    assert_structure(
        copied_rewrites_checked(&[
            renamer(&[key("u"), key("c")], "a"),
            renamer(&[key("u"), key("c")], "b"),
        ]),
        "two ways",
        true,
    );
}

// `k` is renamed again after `x` was renamed to it, so what the router sends as `m` depends on the
// order the renamers run in.
#[test]
fn renamer_of_a_renamed_key_is_ill_formed() {
    assert_structure(
        copied_rewrites_checked(&[
            renamer(&[key("u"), key("x")], "k"),
            renamer(&[key("u"), key("k")], "m"),
        ]),
        "which another renames to",
        true,
    );
}

// A renamer below the alias `a`, run before `a` is named back to `u`: on a copy of `u` it renames
// the demanded `right` away, which idempotence cannot see, the demand having no `a`.
#[test]
fn renamer_below_a_renamed_key_is_ill_formed() {
    assert_structure(
        copied_rewrites_checked(&[
            renamer(&[key("a"), key("right")], "gone"),
            renamer(&[key("a")], "u"),
        ]),
        "two ways",
        true,
    );
}

// A `TypenameEquals` element makes the router rename `r2` for some objects at `u` and not others.
#[test]
fn renamer_filtered_by_type_is_ill_formed() {
    assert_structure(
        copied_rewrites_checked(&[renamer(
            &[
                key("u"),
                r#"{ "TypenameEquals": "U" }"#.to_string(),
                key("r2"),
            ],
            "right",
        )]),
        "tests the type of what it renames",
        true,
    );
}

// So does a type condition on a key: the router follows `u` only where it holds a `U`.
#[test]
fn renamer_with_a_typed_key_is_ill_formed() {
    assert_structure(
        copied_rewrites_checked(&[renamer(
            &[r#"{ "Key": ["u", ["U"]] }"#.to_string(), key("r2")],
            "right",
        )]),
        "tests the type of what it renames",
        true,
    );
}

// Renaming the demanded `right` away sends nothing under two keys, but the subgraph no longer
// receives the `right` it demands.
#[test]
fn demand_renamed_away_is_not_idempotent() {
    let (structure, idempotent) =
        copied_rewrites_checked(&[renamer(&[key("u"), key("right")], "gone")]);
    structure.unwrap();
    assert!(!idempotent);
}

// Renaming `left` onto the demanded `right` takes the demanded `left` away.
#[test]
fn renamer_onto_a_demanded_key_is_not_idempotent() {
    let (structure, idempotent) =
        copied_rewrites_checked(&[renamer(&[key("u"), key("left")], "right")]);
    structure.unwrap();
    assert!(!idempotent);
}

//==================================================================================================
// What an entry must be

// An entry with no type condition is not tied to any entity the router sends, so the checker asks
// every entry for one.
#[test]
fn untyped_unconditional_entry_is_rejected() {
    assert_rejected(
        checked(
            "__typename id",
            &[fragment(None, &[], &key_inputs())],
            "c",
            None,
        ),
        "must carry a type condition",
    );
}

// The same for an untyped entry under a condition.
#[test]
fn untyped_conditioned_top_entry_is_rejected() {
    assert_rejected(
        checked(
            "__typename id a @include(if: $x)",
            &[
                entry(&key_inputs()),
                fragment(None, &[include("x")], &[input("a")]),
            ],
            "c b @include(if: $x)",
            None,
        ),
        "must carry a type condition",
    );
}

// And for an untyped entry wrapping an untyped conditioned fragment.
#[test]
fn untyped_conditioned_nested_entry_is_rejected() {
    assert_rejected(
        checked(
            "__typename id a @include(if: $x)",
            &[
                entry(&key_inputs()),
                fragment(None, &[], &[fragment(None, &[include("x")], &[input("a")])]),
            ],
            "c b @include(if: $x)",
            None,
        ),
        "must carry a type condition",
    );
}

/// A fetch to `one` selecting `{ t { __typename id } }` at the root, with `requires` or without.
fn root_fetch_plan(requires: &[String]) -> String {
    format!(
        r#"{{ "Fetch": {{
            "subgraph_name": "one",
            "variable_usages": [],
            "requires": [{}],
            "operation_document": "{{ t {{ __typename id }} }}",
            "operation_kind": "query",
            "input_rewrites": [],
            "output_rewrites": [],
            "context_rewrites": []
        }} }}"#,
        requires.join(", ")
    )
}

// A fetch with an entry is an entity fetch, needing `_entities`, even when the entry is excluded
// outright. Without the entry the same operation is an ordinary root fetch.
#[test]
fn all_literal_excluded_entries_keep_entity_kind_is_rejected() {
    let fixture = fixture();
    let excluded = fixture.written_plan(&root_fetch_plan(&[fragment(
        Some("T"),
        &[include_literal(false)],
        &key_inputs(),
    )]));
    assert_rejected(
        fixture.check(&excluded, "{ t { id } }"),
        "must select exactly one root field, `_entities`",
    );
    let root = fixture.written_plan(&root_fetch_plan(&[]));
    fixture.check(&root, "{ t { id } }").unwrap();
}

//==================================================================================================
// Literal conditions

// A fragment under `@include(if: false)` is never run, so its `b` owes nothing, and the `a` it
// would require need not be fetched.
#[test]
fn entity_fragment_literal_false_is_accepted() {
    checked(
        "__typename id",
        &[entry(&key_inputs()), entry(&key_inputs())],
        "c",
        Some("... on T { c } ... on T @include(if: false) { b }"),
    )
    .unwrap();
}

/// `a` in an entry under `condition`, for `b` under `directive`, fetched where `fetched_a`.
fn literal_checked(
    condition: String,
    directive: &str,
    fetched_a: bool,
) -> Result<(), CorrectnessError> {
    checked(
        if fetched_a {
            "__typename id a"
        } else {
            "__typename id"
        },
        &[
            entry(&key_inputs()),
            fragment(Some("T"), &[condition], &[input("a")]),
        ],
        &format!("c b {directive}"),
        None,
    )
}

// `@include(if: true)` reads the entry, so the fetched `a` is sent for `b`.
#[test]
fn include_true_reads_the_entry_is_accepted() {
    literal_checked(include_literal(true), "@include(if: true)", true).unwrap();
}

// `@include(if: false)` excludes the entry, so its unfetched `a` is never read, and `b` is never
// asked for.
#[test]
fn include_false_excludes_the_entry_is_accepted() {
    literal_checked(include_literal(false), "@include(if: false)", false).unwrap();
}

#[test]
fn skip_false_reads_the_entry_is_accepted() {
    literal_checked(skip_literal(false), "@skip(if: false)", true).unwrap();
}

#[test]
fn skip_true_excludes_the_entry_is_accepted() {
    literal_checked(skip_literal(true), "@skip(if: true)", false).unwrap();
}

// An entry excluded outright sends no representation, so `two` resolves nothing and `c` is never
// fetched.
#[test]
fn all_literal_excluded_entries_send_nothing_is_rejected() {
    assert_rejected(
        checked(
            "__typename id",
            &[fragment(
                Some("T"),
                &[include_literal(false)],
                &key_inputs(),
            )],
            "c",
            None,
        ),
        "query plan does not fetch everything the operation requests",
    );
}

fn right_excluded_inputs() -> Vec<String> {
    vec![entry(&with_key(&[input_below(
        "u",
        &[fragment(None, &[include_literal(false)], &[input("right")])],
    )]))]
}

// Excluding the fragment below `u` still reads `u` itself, which was fetched.
#[test]
fn nested_literal_exclusion_keeps_containing_field_is_accepted() {
    checked(
        "__typename id u { left }",
        &right_excluded_inputs(),
        "c",
        None,
    )
    .unwrap();
}

// Nor does the exclusion excuse reading a `u` the plan never fetched.
#[test]
fn nested_literal_exclusion_cannot_erase_containing_read_is_rejected() {
    assert_rejected(
        checked("__typename id", &right_excluded_inputs(), "c", None),
        "reads a key the plan has not fetched",
    );
}

//==================================================================================================
// Variable conditions

// `@include(if: $x) @skip(if: $x)` holds nowhere, so the entry's unfetched `a` is never read.
#[test]
fn resolved_opposite_conditions_exclude_the_entry_is_accepted() {
    checked(
        "__typename id",
        &[
            entry(&key_inputs()),
            fragment(Some("T"), &[include("x"), skip("x")], &[input("a")]),
        ],
        "c",
        None,
    )
    .unwrap();
}

fn nested_inputs() -> Vec<String> {
    vec![
        entry(&key_inputs()),
        fragment(
            Some("T"),
            &[include("x")],
            &[fragment(None, &[include("y")], &[input("a")])],
        ),
    ]
}

// Nested conditions are conjoined: `a` is read, and sent for `b`, only where `x` and `y` both
// hold, which is where it was fetched.
#[test]
fn nested_independent_conditions_are_conjoined_is_accepted() {
    checked(
        "__typename id ... @include(if: $x) { ... @include(if: $y) { a } }",
        &nested_inputs(),
        "c ... @include(if: $x) { ... @include(if: $y) { b } }",
        None,
    )
    .unwrap();
}

// Where `y` holds and `x` does not, `b` needs an `a` the outer condition keeps the entry from
// sending.
#[test]
fn nested_outer_exclusion_cannot_cover_y_demand_is_rejected() {
    assert_rejected(
        checked(
            "__typename id a",
            &nested_inputs(),
            "c b @include(if: $y)",
            None,
        ),
        "the entries do not send what the subgraph demands",
    );
}

// Where `x` holds and `y` does not, `b` needs an `a` the inner condition keeps the entry from
// sending.
#[test]
fn nested_inner_exclusion_cannot_cover_x_demand_is_rejected() {
    assert_rejected(
        checked(
            "__typename id a",
            &nested_inputs(),
            "c b @include(if: $x)",
            None,
        ),
        "the entries do not send what the subgraph demands",
    );
}

/// `c` unconditionally, and `b` in a second `T` fragment under `directive`.
fn guarded_cases(directive: &str) -> String {
    format!("... on T {{ c }} ... on T {directive} {{ b }}")
}

// The `_entities` fragment's own `@include(if: $x)` means `b`, and the `a` it requires, are only
// asked for where `x` holds, which is where the entry sends `a`.
#[test]
fn entity_fragment_directive_limits_demand_is_accepted() {
    checked(
        "__typename id a @include(if: $x)",
        &[
            entry(&key_inputs()),
            fragment(Some("T"), &[include("x")], &[input("a")]),
        ],
        "c b @include(if: $x)",
        Some(&guarded_cases("@include(if: $x)")),
    )
    .unwrap();
}

// Where `x` holds and `y` does not, the fragment asks for `b`, whose `a` the entry does not send.
#[test]
fn entity_fragment_admits_configuration_its_entry_excludes_is_rejected() {
    assert_rejected(
        checked(
            "__typename id a",
            &[
                entry(&key_inputs()),
                fragment(Some("T"), &[include("y")], &[input("a")]),
            ],
            "c b @include(if: $x)",
            Some(&guarded_cases("@include(if: $x)")),
        ),
        "the entries do not send what the subgraph demands",
    );
}

// The same with `@skip(if: $x)`, owing `a` only where `x` does not hold.
#[test]
fn entity_fragment_skip_limits_demand_is_accepted() {
    checked(
        "__typename id a @skip(if: $x)",
        &[
            entry(&key_inputs()),
            fragment(Some("T"), &[skip("x")], &[input("a")]),
        ],
        "c b @skip(if: $x)",
        Some(&guarded_cases("@skip(if: $x)")),
    )
    .unwrap();
}

//==================================================================================================
// Configurations of the entries' conditions

/// The key, and an entry sending `a` under `@include(if: $name)` for each of `names`.
fn growth_entries(names: &[&str]) -> Vec<String> {
    let mut out = vec![entry(&key_inputs())];
    out.extend(
        names
            .iter()
            .map(|name| fragment(Some("T"), &[include(name)], &[input("a")])),
    );
    out
}

/// How many configurations of `entries`' conditions a case is checked in.
fn configuration_count(entries: &[String]) -> usize {
    let entries: Vec<requires_selection::Selection> = entries
        .iter()
        .map(|entry| serde_json::from_str(entry).expect("valid entry json"))
        .collect();
    entry_configurations(&entries.iter().collect::<Vec<_>>()).len()
}

/// `growth_entries`, with `a` fetched where `fetched`, for a client asking for `c`.
fn growth_checked(names: &[&str], fetched: bool) -> Result<(), CorrectnessError> {
    checked(
        if fetched {
            "__typename id a"
        } else {
            "__typename id"
        },
        &growth_entries(names),
        "c",
        None,
    )
}

// What the router sends changes only with the variables the entries read, so more entries on `x`
// add no configuration.
#[test]
fn fixed_variable_entries_keep_two_configurations_is_accepted() {
    assert_eq!(configuration_count(&growth_entries(&["x", "x", "x"])), 2);
    assert_eq!(
        configuration_count(&growth_entries(&["x", "x", "x", "x", "x", "x"])),
        2
    );
    growth_checked(&["x", "x", "x", "x", "x", "x"], true).unwrap();
}

// Each independent variable doubles the configurations, though `c` itself has no directive.
#[test]
fn independent_entry_variables_add_configuration_axes_is_accepted() {
    assert_eq!(configuration_count(&growth_entries(&["x"])), 2);
    assert_eq!(configuration_count(&growth_entries(&["x", "y"])), 4);
    assert_eq!(configuration_count(&growth_entries(&["x", "y", "z"])), 8);
    growth_checked(&["x", "y", "z"], true).unwrap();
}

// The entries read an unfetched `a` wherever their variable holds, which no demand excuses.
#[test]
fn rejected_read_keeps_entry_configuration_axes_is_rejected() {
    assert_eq!(configuration_count(&growth_entries(&["x", "y"])), 4);
    assert_rejected(
        growth_checked(&["x", "y"], false),
        "reads a key the plan has not fetched",
    );
}

fn nested_excluded_inputs() -> Vec<String> {
    vec![
        entry(&key_inputs()),
        fragment(
            Some("T"),
            &[include_literal(false), include("x")],
            &[fragment(None, &[include("y")], &[input("a")])],
        ),
    ]
}

// `@include(if: false)` excludes the entry, so its unfetched `a` is never read, yet the variables
// behind it still count as configuration axes.
#[test]
fn nested_excluded_literals_keep_both_configuration_axes_is_accepted() {
    assert_eq!(configuration_count(&nested_excluded_inputs()), 4);
    checked("__typename id", &nested_excluded_inputs(), "c", None).unwrap();
}

// A nullable `$x` with a default is still a Boolean wherever the entry reads it, and the entry's
// opposite conditions on it hold nowhere.
#[test]
fn defaulted_boolean_passes_the_boundary_is_accepted() {
    let fixture = fixture();
    let plan = plan(
        &fixture,
        "__typename id",
        &[
            entry(&key_inputs()),
            fragment(Some("T"), &[include("x"), skip("x")], &[input("a")]),
        ],
        "... on T { c }",
    );
    fixture
        .check(
            &plan,
            "query($x: Boolean = false) { t { c ... @include(if: $x) { c } } }",
        )
        .unwrap();
}
