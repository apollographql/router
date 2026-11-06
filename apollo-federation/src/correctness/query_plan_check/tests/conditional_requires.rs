//! Conditional `requires`: coverage, copies, overwrites and configurations.
//!
//! Ports of the model's `ConditionalRequires` tests. Most plans are one shape: `one` fetches what
//! `t` holds, then a second subgraph is sent the `requires` entries and asked for each `T` under
//! `_entities`. An object's representation is every entry admitting it, merged, and its entries'
//! `@skip`/`@include` decide what it holds in each configuration of their variables, so a case is
//! checked once per configuration, with a key chosen for each.

use apollo_compiler::Name;

use super::CorrectnessError;
use super::Fixture;
use crate::query_plan::QueryPlan;
use crate::query_plan::requires_selection;
use crate::query_plan::requires_selection::Condition;
use crate::query_plan::requires_selection::ConditionKind;
use crate::query_plan::requires_selection::ConditionValue;

/// `one` fetches `t`; `two`, `twokeys` and `projections` resolve a `T` by key, with different
/// keys and `@requires`.
fn fixture() -> Fixture {
    Fixture::new(include_str!("../testdata/conditional_requires.graphql"))
}

//==================================================================================================
// Writing entries

type Input = requires_selection::Selection;

/// A field an entry reads, and copies whole if it is composite.
fn input(name: &str) -> Input {
    object(name, Vec::new())
}

fn object(name: &str, selections: Vec<Input>) -> Input {
    Input::Field(requires_selection::Field {
        alias: None,
        name: Name::new(name).unwrap(),
        selections,
    })
}

fn fragment(
    type_condition: Option<&str>,
    conditions: Vec<Condition>,
    selections: Vec<Input>,
) -> Input {
    Input::InlineFragment(requires_selection::InlineFragment {
        type_condition: type_condition.map(|name| Name::new(name).unwrap()),
        conditions,
        selections,
    })
}

/// An entry, or a fragment inside one, on `T`.
fn on_t(selections: Vec<Input>) -> Input {
    fragment(Some("T"), Vec::new(), selections)
}

/// [`on_t`], read only where `conditions` hold.
fn on_t_when(conditions: Vec<Condition>, selections: Vec<Input>) -> Input {
    fragment(Some("T"), conditions, selections)
}

fn include(variable: &str) -> Condition {
    Condition {
        kind: ConditionKind::Include,
        if_: ConditionValue::Variable(Name::new(variable).unwrap()),
    }
}

fn skip(variable: &str) -> Condition {
    Condition {
        kind: ConditionKind::Skip,
        if_: ConditionValue::Variable(Name::new(variable).unwrap()),
    }
}

/// `T`'s key, `__typename id`, followed by `rest`.
fn key_and(rest: Vec<Input>) -> Vec<Input> {
    let mut inputs = vec![input("__typename"), input("id")];
    inputs.extend(rest);
    inputs
}

fn u_left() -> Input {
    object("u", vec![input("left")])
}

fn u_right() -> Input {
    object("u", vec![input("right")])
}

//==================================================================================================
// Writing plans

/// The declarations of the Boolean variables `body` uses, each prefixed with `prefix`.
fn declarations(prefix: &str, body: &str) -> String {
    ["x", "y", "test"]
        .iter()
        .filter(|variable| body.contains(&format!("${variable}")))
        .map(|variable| format!("{prefix}${variable}: Boolean!"))
        .collect()
}

/// `body` as an operation, declaring the variables it uses.
fn operation(body: &str) -> String {
    let declared = declarations(", ", body);
    match declared.strip_prefix(", ") {
        Some(declared) => format!("query({declared}) {body}"),
        None => body.to_string(),
    }
}

fn fetch(subgraph: &str, requires: &str, input_rewrites: &str, operation: &str) -> String {
    format!(
        r#"{{ "Fetch": {{
            "subgraph_name": "{subgraph}",
            "variable_usages": [],
            "requires": {requires},
            "operation_document": "{operation}",
            "operation_kind": "query",
            "input_rewrites": [{input_rewrites}],
            "output_rewrites": [],
            "context_rewrites": []
        }} }}"#
    )
}

/// The fetch to `one` selecting `fetched` under `t`.
fn fetch_t(fetched: &str) -> String {
    fetch(
        "one",
        "[]",
        "",
        &operation(&format!("{{ t {{ {fetched} }} }}")),
    )
}

/// An entity fetch to `subgraph` at `t`, sent `entries` renamed by `input_rewrites`, asked for
/// `cases` under `_entities`.
fn entity_fetch_at_t(
    subgraph: &str,
    entries: &[Input],
    input_rewrites: &str,
    cases: &str,
) -> String {
    let entities = format!(
        "query($representations: [_Any!]!{}) \
         {{ _entities(representations: $representations) {{ {cases} }} }}",
        declarations(", ", cases)
    );
    let requires = serde_json::to_string(entries).unwrap();
    format!(
        r#"{{ "Flatten": {{
            "path": [{{ "Key": ["t", null] }}],
            "node": {}
        }} }}"#,
        fetch(subgraph, &requires, input_rewrites, &entities)
    )
}

/// `one` fetches `fetched` under `t`, then `subgraph` is sent `entries` renamed by
/// `input_rewrites` and asked for `cases`.
fn plan(
    fixture: &Fixture,
    fetched: &str,
    subgraph: &str,
    entries: &[Input],
    input_rewrites: &str,
    cases: &str,
) -> QueryPlan {
    fixture.written_plan(&format!(
        r#"{{ "Sequence": {{ "nodes": [{}, {}] }} }}"#,
        fetch_t(fetched),
        entity_fetch_at_t(subgraph, entries, input_rewrites, cases)
    ))
}

/// [`plan`] for a client asking for `selected` under `t`, its cases one `T` fragment selecting
/// the same unless given.
fn checked_by(
    subgraph: &str,
    fetched: &str,
    entries: &[Input],
    selected: &str,
    cases: Option<&str>,
) -> Result<(), CorrectnessError> {
    let fixture = fixture();
    let cases = cases.map_or_else(|| format!("... on T {{ {selected} }}"), str::to_string);
    let plan = plan(&fixture, fetched, subgraph, entries, "", &cases);
    fixture.check(&plan, &operation(&format!("{{ t {{ {selected} }} }}")))
}

/// [`checked_by`] with `two`, which resolves a `T` by `id`, its `b`, `d` and `both` requiring `a`,
/// `e` and `u { left right }`.
fn checked(
    fetched: &str,
    entries: &[Input],
    selected: &str,
    cases: Option<&str>,
) -> Result<(), CorrectnessError> {
    checked_by("two", fetched, entries, selected, cases)
}

/// [`checked_by`] with `projections`, whose `b` requires `u { right }` and `d` `u { left }`.
fn checked_projections(
    fetched: &str,
    entries: &[Input],
    selected: &str,
) -> Result<(), CorrectnessError> {
    checked_by("projections", fetched, entries, selected, None)
}

#[track_caller]
fn assert_rejected(result: Result<(), CorrectnessError>, reason: &str) {
    match result {
        Ok(()) => panic!("expected the check to reject this plan"),
        Err(error) => {
            let error = error.to_string();
            assert!(error.contains(reason), "{error}");
        }
    }
}

const KEY: &str = "__typename id";
const KEY_A: &str = "__typename id a";
/// `a` fetched only where `x` holds.
const GUARDED_A: &str = "__typename id a @include(if: $x)";
/// `b`, and the `a` it requires, asked for only where `x` holds.
const GUARDED_B: &str = "c b @include(if: $x)";
const KEY_LEFT_RIGHT: &str = "__typename id u { left right }";

//==================================================================================================
// One entry or several, conditioned or not

/// The planner's plan for `c` and `b @include(if: $test)`, fetching `a` only where `test` holds.
fn planner_plan(entries: &[Input]) -> Result<(), CorrectnessError> {
    let selected = "c b @include(if: $test)";
    checked(
        &format!("{KEY} ... on T @include(if: $test) {{ a }}"),
        entries,
        selected,
        None,
    )
}

// The planner's shape for a conditional `@requires`: the key sent always, and `a` in a second
// entry read under the same condition as the `b` that requires it.
#[test]
fn planner_conditional_requires_sibling_is_accepted() {
    planner_plan(&[
        on_t(key_and(vec![])),
        on_t_when(vec![include("test")], vec![input("__typename"), input("a")]),
    ])
    .unwrap();
}

// Without the conditional entry, `b` is resolved where `test` holds from a representation with
// no `a`.
#[test]
fn planner_conditional_entry_removed_is_rejected() {
    assert_rejected(
        planner_plan(&[on_t(key_and(vec![]))]),
        "the entries do not send what the subgraph demands",
    );
}

fn split_inputs() -> Vec<Input> {
    vec![
        on_t(key_and(vec![])),
        on_t_when(vec![include("x")], vec![input("a")]),
    ]
}

// `b` is asked for always, but its `a` is sent only where `x` holds.
#[test]
fn narrower_entry_leaves_unconditional_demand_is_rejected() {
    assert_rejected(
        checked(KEY_A, &split_inputs(), "b", None),
        "the entries do not send what the subgraph demands",
    );
}

// Sending a fetched `a` where `b` is not asked for is harmless.
#[test]
fn coverage_with_available_surplus_is_accepted() {
    checked(KEY_A, &[on_t(key_and(vec![input("a")]))], GUARDED_B, None).unwrap();
}

// The entry reads `a` always, but `one` fetched it only where `x` holds: where it fails, the
// router builds the representation from a key that is not there.
#[test]
fn coverage_with_unavailable_surplus_is_rejected() {
    assert_rejected(
        checked(
            GUARDED_A,
            &[on_t(key_and(vec![input("a")]))],
            GUARDED_B,
            None,
        ),
        "reads a key the plan has not fetched",
    );
}

// A fetched `a` nothing demands may still be sent.
#[test]
fn unconditional_surplus_is_accepted() {
    checked(KEY_A, &[on_t(key_and(vec![input("a")]))], "c", None).unwrap();
}

// Both entries are on `T`, so the router merges them into one representation holding the key and
// `a` together.
#[test]
fn split_unconditional_representation_is_accepted() {
    checked(
        KEY_A,
        &[on_t(key_and(vec![])), on_t(vec![input("a")])],
        "b",
        None,
    )
    .unwrap();
}

// The same split under `x`, where `b` asks for `a` and `one` fetched it.
#[test]
fn split_conditioned_representation_is_accepted() {
    checked(GUARDED_A, &split_inputs(), GUARDED_B, None).unwrap();
}

// Where `x` holds the only entry is read, and it sends no `id` to resolve the `T` by.
#[test]
fn conditioned_entry_alone_lacks_key_is_rejected() {
    assert_rejected(
        checked(
            GUARDED_A,
            &[on_t_when(
                vec![include("x")],
                vec![input("__typename"), input("a")],
            )],
            "b @include(if: $x)",
            None,
        ),
        "the entries do not send what the subgraph demands",
    );
}

// Where `x` fails nothing is asked for, and where it holds the entry sends all of `b`'s demand.
#[test]
fn conditioned_entry_with_key_is_accepted() {
    checked(
        GUARDED_A,
        &[on_t_when(vec![include("x")], key_and(vec![input("a")]))],
        "b @include(if: $x)",
        None,
    )
    .unwrap();
}

//==================================================================================================
// Overwrites within an entry

// The later field `u { left }` replaces the value the fragment's `u { right }` wrote, as the
// router's `execute_selection_set` writes a field over its key.
#[test]
fn unconditional_later_field_replacement_is_rejected() {
    assert_rejected(
        checked(
            KEY_LEFT_RIGHT,
            &[on_t(key_and(vec![on_t(vec![u_right()]), u_left()]))],
            "both",
            None,
        ),
        "overwrites `u`",
    );
}

// A later fragment deep-merges its `u { right }` into `u { left }`, so `both` is sent both.
#[test]
fn unconditional_later_fragment_merge_is_accepted() {
    checked(
        KEY_LEFT_RIGHT,
        &[on_t(key_and(vec![u_left(), on_t(vec![u_right()])]))],
        "both",
        None,
    )
    .unwrap();
}

fn overwritten_inputs() -> Vec<Input> {
    vec![on_t(key_and(vec![on_t(vec![u_right()]), u_left()]))]
}

// The entry reads `u { right }`, which was never fetched, before overwriting it.
#[test]
fn overwritten_unfetched_read_is_rejected() {
    assert_rejected(
        checked_projections("__typename id u { left }", &overwritten_inputs(), "d"),
        "overwrites `u`",
    );
}

// Fetching `u { right }` does not help: any field overwriting what its entry read is rejected,
// so the representation's shape never depends on the order the router writes it in.
#[test]
fn overwritten_fetched_read_is_rejected() {
    assert_rejected(
        checked_projections(KEY_LEFT_RIGHT, &overwritten_inputs(), "d"),
        "overwrites `u`",
    );
}

/// `d` always, which requires `u { left }`, and `b` where `x` holds, which requires
/// `u { right }`.
const PROJECTION_DEMAND: &str = "d b @include(if: $x)";

// Where `x` holds, the later `u { left }` replaces the conditioned `u { right }` that `b` needs.
#[test]
fn later_field_replaces_conditioned_projection_is_rejected() {
    assert_rejected(
        checked_projections(
            KEY_LEFT_RIGHT,
            &[on_t(key_and(vec![
                on_t_when(vec![include("x")], vec![u_right()]),
                u_left(),
            ]))],
            PROJECTION_DEMAND,
        ),
        "overwrites `u`",
    );
}

// The later conditioned fragment merges `u { right }` in where `x` holds, beside `u { left }`.
#[test]
fn later_conditioned_fragment_merges_projection_is_accepted() {
    checked_projections(
        KEY_LEFT_RIGHT,
        &[on_t(key_and(vec![
            u_left(),
            on_t_when(vec![include("x")], vec![u_right()]),
        ]))],
        PROJECTION_DEMAND,
    )
    .unwrap();
}

//==================================================================================================
// A composite field copied whole

fn copied_inputs() -> Vec<Input> {
    vec![on_t(key_and(vec![input("u")]))]
}

// A bare `u` copies the fetched `extra: left` beside the demanded `right`; the subgraph ignores
// the extra key.
#[test]
fn copied_composite_surplus_alias_is_accepted() {
    checked_projections(
        "__typename id u { right extra: left }",
        &copied_inputs(),
        "b",
    )
    .unwrap();
}

// The copy holds only `extra`, so `b`'s `u { right }` is never sent.
#[test]
fn copied_composite_surplus_missing_demand_is_rejected() {
    assert_rejected(
        checked_projections("__typename id u { extra: left }", &copied_inputs(), "b"),
        "the plan has not fetched what the subgraph demands",
    );
}

// The copy's `right` holds `left`'s value, which is not what `b` demands under that key.
#[test]
fn copied_composite_surplus_misidentified_demand_is_rejected() {
    assert_rejected(
        checked_projections(
            "__typename id u { right: left extra: left }",
            &copied_inputs(),
            "b",
        ),
        "the plan has not fetched what the subgraph demands",
    );
}

/// A key renamer from `from`, a path of keys, to `to`.
fn key_renamer(from: &[&str], to: &str) -> String {
    let path = from
        .iter()
        .map(|key| format!(r#"{{ "Key": ["{key}", null] }}"#))
        .collect::<Vec<_>>()
        .join(", ");
    format!(r#"{{ "KeyRenamer": {{ "path": [{path}], "rename_key_to": "{to}" }} }}"#)
}

/// `one` fetches `fetched` under `t`, then `projections` is sent the copied `u`, renamed by
/// `input_rewrites`, and asked for `b`.
fn copied_plan(fixture: &Fixture, fetched: &str, input_rewrites: &str) -> QueryPlan {
    plan(
        fixture,
        fetched,
        "projections",
        &copied_inputs(),
        input_rewrites,
        "... on T { b }",
    )
}

// The copied `r: right` is named back to `right`, and the copied `extra: left` needs no rename.
#[test]
fn copied_demand_renamed_back_with_surplus_is_accepted() {
    let fixture = fixture();
    let plan = copied_plan(
        &fixture,
        "__typename id u { r: right extra: left }",
        &key_renamer(&["u", "r"], "right"),
    );
    fixture.check(&plan, "{ t { b } }").unwrap();
}

// The copied `right` is renamed to `gone`, so the subgraph is never sent the `right` it demands.
#[test]
fn copied_demand_renamed_away_is_rejected() {
    let fixture = fixture();
    let plan = copied_plan(
        &fixture,
        "__typename id u { right }",
        &key_renamer(&["u", "right"], "gone"),
    );
    assert_rejected(
        fixture.check(&plan, "{ t { b } }"),
        "the input rewrites rename a key the subgraph demands",
    );
}

// The same renamer on a fetch run only where `x` fails, where `b` is not asked for. A rewrite
// applies whatever the conditions, so it still renames a key the subgraph demands; the plan is
// otherwise complete.
#[test]
fn copied_inactive_demand_renamed_away_is_rejected() {
    let fixture = fixture();
    let entity_fetch = entity_fetch_at_t(
        "projections",
        &copied_inputs(),
        &key_renamer(&["u", "right"], "gone"),
        "... on T { c b @include(if: $x) }",
    );
    let plan = fixture.written_plan(&format!(
        r#"{{ "Sequence": {{ "nodes": [
            {},
            {{ "Condition": {{
                "condition_variable": "x",
                "if_clause": null,
                "else_clause": {entity_fetch}
            }} }}
        ] }} }}"#,
        fetch_t("__typename id u { right }"),
    ));
    assert_rejected(
        fixture.check(&plan, "query($x: Boolean!) { t { c @skip(if: $x) } }"),
        "the input rewrites rename a key the subgraph demands",
    );
}

//==================================================================================================
// A key per configuration

fn opposite_inputs() -> Vec<Input> {
    vec![
        on_t_when(vec![include("x")], key_and(vec![input("a")])),
        on_t_when(vec![skip("x")], key_and(vec![input("a")])),
    ]
}

// Whatever `x` is, one of the two entries is read and sends all of `b`'s demand.
#[test]
fn complementary_entries_cover_both_configurations_is_accepted() {
    checked(KEY_A, &opposite_inputs(), "b", None).unwrap();
}

// Without the `@skip` entry, the router sends no representation where `x` fails, so nothing is
// owed there and nothing is fetched there either, though `b` is asked for.
#[test]
fn complementary_entry_removed_is_rejected() {
    assert_rejected(
        checked(
            KEY_A,
            &[on_t_when(vec![include("x")], key_and(vec![input("a")]))],
            "b",
            None,
        ),
        "query plan does not fetch everything the operation requests",
    );
}

fn key_per_configuration_inputs() -> Vec<Input> {
    vec![
        on_t_when(vec![include("x")], key_and(vec![])),
        on_t_when(vec![skip("x")], vec![input("__typename"), input("id2")]),
    ]
}

// The representation carries `id` where `x` holds and `id2` where it fails, and `twokeys`
// resolves each by its own key, though no one key serves both.
#[test]
fn complementary_entries_may_resolve_different_keys_is_accepted() {
    checked_by(
        "twokeys",
        "__typename id id2",
        &key_per_configuration_inputs(),
        "c",
        None,
    )
    .unwrap();
}

// `two` resolves a `T` only by `id`, which the representation sent where `x` fails lacks.
#[test]
fn configuration_without_a_key_is_rejected() {
    assert_rejected(
        checked(
            "__typename id id2",
            &key_per_configuration_inputs(),
            "c",
            None,
        ),
        "is not met for `T` when ¬$x",
    );
}

//==================================================================================================
// A key per implementation

/// `one` lists `I`s, implemented by `A` and `B`; `bothkeys`, `idkey` and `compoundkey` declare `I`
/// as an interface object resolved by `id` or `sku`, by `id`, and by `id sku`.
fn implementation_keys() -> Fixture {
    Fixture::new(include_str!("../testdata/implementation_keys.graphql"))
}

/// `one` fetches every item's `__typename id sku`, then `subgraph` is sent `id` for an `A` and
/// `b_key` for a `B`, and asked for each `I`.
fn keyed_checked(subgraph: &str, b_key: &str) -> Result<(), CorrectnessError> {
    let fixture = implementation_keys();
    let entries = [
        fragment(Some("A"), Vec::new(), key_and(vec![])),
        fragment(
            Some("B"),
            Vec::new(),
            vec![input("__typename"), input(b_key)],
        ),
    ];
    let requires = serde_json::to_string(&entries).unwrap();
    let plan = fixture.written_plan(&format!(
        r#"{{ "Sequence": {{ "nodes": [
            {},
            {{ "Flatten": {{
                "path": [{{ "Key": ["items", null] }}, {{ "AnyIndex": null }}],
                "node": {}
            }} }}
        ] }} }}"#,
        fetch("one", "[]", "", "{ items { __typename id sku } }"),
        fetch(
            subgraph,
            &requires,
            "",
            "query($representations: [_Any!]!) \
             { _entities(representations: $representations) { ... on I { __typename } } }",
        ),
    ));
    fixture.check(&plan, "{ items { __typename } }")
}

// `I` has two keys; an `A` is sent `id` and a `B` `sku`, and each resolves by the key it carries.
#[test]
fn implementations_sent_different_keys_are_accepted() {
    keyed_checked("bothkeys", "sku").unwrap();
}

// Sending a `B` the same `id` as an `A` resolves both through one key.
#[test]
fn implementations_sent_one_key_are_accepted() {
    keyed_checked("idkey", "id").unwrap();
}

// With the one compound key `id sku`, each implementation is sent only part of it.
#[test]
fn implementations_sent_part_of_a_compound_key_are_rejected() {
    assert_rejected(
        keyed_checked("compoundkey", "sku"),
        "the entries do not send what the subgraph demands",
    );
}

//==================================================================================================
// Entries and entity cases on other types

// The entry on `I` admits a `T` too, so the router merges its `a` into the representation the
// entry on `T` starts.
#[test]
fn overlapping_interface_and_object_entries_are_accepted() {
    checked(
        KEY_A,
        &[
            on_t(key_and(vec![])),
            fragment(Some("I"), Vec::new(), vec![input("a")]),
        ],
        "b",
        None,
    )
    .unwrap();
}

// The entity fragment's `@include(if: $x)` limits what is demanded, not what the entry reads:
// the router builds the representation, `a` and all, wherever `x` fails too.
#[test]
fn entity_fragment_cannot_hide_unread_surplus_is_rejected() {
    assert_rejected(
        checked(
            GUARDED_A,
            &[on_t(key_and(vec![input("a")]))],
            "b @include(if: $x)",
            Some("... on T @include(if: $x) { b }"),
        ),
        "reads a key the plan has not fetched",
    );
}

const TWO_CASES: &str = "... on T { b } ... on T { d }";

// Two fragments on `T` are one case, so one representation must carry `a` and `e` together.
#[test]
fn same_type_fragment_demands_together_are_accepted() {
    checked(
        "__typename id a e",
        &[on_t(key_and(vec![input("a"), input("e")]))],
        "b d",
        Some(TWO_CASES),
    )
    .unwrap();
}

// The second fragment's `d` requires an `e` the representation does not carry.
#[test]
fn same_type_fragment_demand_missing_is_rejected() {
    assert_rejected(
        checked(
            "__typename id a e",
            &[on_t(key_and(vec![input("a")]))],
            "b d",
            Some(TWO_CASES),
        ),
        "the entries do not send what the subgraph demands",
    );
}

//==================================================================================================
// Sibling entries on two variables

/// `b` asked where `x` holds and `c` where `y` holds, each case under the same condition, sent
/// `entries`. `include` picks `@include` or `@skip` throughout.
fn check_two_variables(include_kind: bool, entries: &[Input]) -> Result<(), CorrectnessError> {
    let directive = if include_kind { "include" } else { "skip" };
    let selected = format!("b @{directive}(if: $x) c @{directive}(if: $y)");
    let cases =
        format!("... on T @{directive}(if: $x) {{ b }} ... on T @{directive}(if: $y) {{ c }}");
    checked(KEY_A, entries, &selected, Some(&cases))
}

/// An entry on `T` read where `x` holds, with the key and `b`'s `a`, and one read where `y` holds,
/// with the key alone. `include` picks `@include` or `@skip`.
fn two_variable_entries(include_kind: bool) -> Vec<Input> {
    let condition = if include_kind { include } else { skip };
    vec![
        on_t_when(vec![condition("x")], key_and(vec![input("a")])),
        on_t_when(vec![condition("y")], key_and(vec![])),
    ]
}

// Each case is sent a representation exactly where it is asked, and where neither variable lets
// one through nothing is asked.
#[test]
fn sibling_entries_on_two_variables_are_accepted() {
    check_two_variables(true, &two_variable_entries(true)).unwrap();
}

// The same with `@skip`.
#[test]
fn sibling_skipped_entries_on_two_variables_are_accepted() {
    check_two_variables(false, &two_variable_entries(false)).unwrap();
}

// Without the entry read where `y` holds, no representation is sent where `y` holds and `x` does
// not, so nothing fetches the `c` asked there.
#[test]
fn sibling_entry_on_the_second_variable_removed_is_rejected() {
    assert_rejected(
        check_two_variables(true, &two_variable_entries(true)[..1]),
        "query plan does not fetch everything the operation requests",
    );
}

//==================================================================================================
// One entry, two keys

/// One entry carrying the fields of both of `T`'s keys, `id` and `id2`.
fn both_keys_entry() -> Vec<Input> {
    vec![on_t(key_and(vec![input("id2")]))]
}

// `twokeys` resolves a `T` by either key, and the representation holds both in full. The planner
// builds such an entry by merging two paths' inputs into one group (`FetchInputs::add`).
#[test]
fn entry_with_two_keys_is_accepted() {
    checked_by(
        "twokeys",
        "__typename id id2",
        &both_keys_entry(),
        "c",
        None,
    )
    .unwrap();
}

// `two` resolves a `T` by `id` alone; `id2` beside it is surplus.
#[test]
fn entry_with_a_surplus_key_is_accepted() {
    checked("__typename id id2", &both_keys_entry(), "c", None).unwrap();
}

// `two` cannot resolve a `T` sent `id2` alone.
#[test]
fn entry_with_only_another_key_is_rejected() {
    assert_rejected(
        checked(
            "__typename id id2",
            &[on_t(vec![input("__typename"), input("id2")])],
            "c",
            None,
        ),
        "is not met for `T`",
    );
}

//==================================================================================================
// Overwriting a scalar

// A scalar `a` is written inside a fragment read where `x` holds, then again as a plain field. The
// second write loses nothing here, but an overwrite is rejected whatever it overwrites, as the
// model's `RequiresSet.overwrittenKeys` is, scalars included. The planner emits this shape when
// it merges a conditioned key's fields with the same field read unconditionally.
#[test]
fn scalar_overwritten_after_a_conditioned_fragment_is_rejected() {
    assert_rejected(
        checked(
            KEY_A,
            &[on_t(key_and(vec![
                on_t_when(vec![include("x")], vec![input("a")]),
                input("a"),
            ]))],
            "b",
            None,
        ),
        "overwrites `a`",
    );
}
