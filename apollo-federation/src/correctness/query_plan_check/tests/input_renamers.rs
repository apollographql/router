//! Input key renamers, read per runtime type.
//!
//! Ports of the model's tests of that name, on the two plans from router PR #10783's review. A
//! product's `event` is a `Match` or a `Race`, and `req`'s `cost` requires `event { team { f } }`.
//! The router builds a representation through the fragments that apply to each object, and renames
//! a key only where `obj.remove(k)` finds it, so a renamer for an alias read under `... on Match`
//! leaves a `Race` event alone. Each plan is `sched` fetching what is asked, then `req` sent the
//! entry under `products.@`, written inline as JSON.

use serde_json::Value;
use serde_json::json;

use super::CorrectnessError;
use super::Fixture;

/// `sched`'s `Match` holds a `team: Club` and a `Race` a `team: Crew`.
fn per_case_renamer() -> Fixture {
    Fixture::new(include_str!("../testdata/per_case_renamer.graphql"))
}

/// `sched`'s `Match` and `Race` each hold a `team: Club`, whose `f` and `g` it serves.
fn renamers_onto_one_key() -> Fixture {
    Fixture::new(include_str!("../testdata/renamers_onto_one_key.graphql"))
}

fn key(name: &str) -> Value {
    json!({ "Key": [name, null] })
}

fn field(name: &str, alias: Option<&str>, selections: &[Value]) -> Value {
    let mut field = json!({ "kind": "Field", "name": name });
    if let Some(alias) = alias {
        field["alias"] = json!(alias);
    }
    if !selections.is_empty() {
        field["selections"] = json!(selections);
    }
    field
}

fn on(type_condition: &str, selections: &[Value]) -> Value {
    json!({ "kind": "InlineFragment", "typeCondition": type_condition, "selections": selections })
}

fn renamer(path: &[&str], rename_key_to: &str) -> Value {
    json!({ "KeyRenamer": {
        "path": path.iter().map(|name| key(name)).collect::<Vec<_>>(),
        "rename_key_to": rename_key_to
    } })
}

/// `sched` fetching `fetched` under `products`, renamed by `output_rewrites`, then `req` sent the
/// entry `... on Product { __typename id event { <event> } }` with `input_rewrites`, for `cost`.
fn cost_checked(
    fixture: &Fixture,
    fetched: &str,
    output_rewrites: &[Value],
    event: &[Value],
    input_rewrites: &[Value],
    operation: &str,
) -> Result<(), CorrectnessError> {
    let entry = on(
        "Product",
        &[
            field("__typename", None, &[]),
            field("id", None, &[]),
            field("event", None, event),
        ],
    );
    let plan = json!({ "Sequence": { "nodes": [
        { "Fetch": {
            "subgraph_name": "sched",
            "variable_usages": [],
            "operation_document": format!("{{ products {{ __typename id event {{ __typename {fetched} }} }} }}"),
            "operation_kind": "query",
            "input_rewrites": [],
            "output_rewrites": output_rewrites,
            "context_rewrites": []
        } },
        { "Flatten": {
            "path": [key("products"), { "AnyIndex": null }],
            "node": { "Fetch": {
                "subgraph_name": "req",
                "variable_usages": [],
                "requires": [entry],
                "operation_document": "query($representations: [_Any!]!) { _entities(representations: $representations) { ... on Product { cost } } }",
                "operation_kind": "query",
                "input_rewrites": input_rewrites,
                "output_rewrites": [],
                "context_rewrites": []
            } }
        } }
    ] } });
    fixture.check(&fixture.written_plan(&plan.to_string()), operation)
}

#[track_caller]
fn assert_rejected(result: Result<(), CorrectnessError>, reason: &str) {
    let error = result
        .expect_err("expected the check to reject this plan")
        .to_string();
    assert!(error.contains(reason), "{error}");
}

//==================================================================================================
// One type condition aliased

/// `team` read under `team__alias_0` where the event is a `Match`, and under its own name where it
/// is a `Race`.
fn team_aliased_on_match() -> Vec<Value> {
    let team_f = |alias| field("team", alias, &[field("f", None, &[])]);
    vec![
        on("Match", &[team_f(Some("team__alias_0"))]),
        on("Race", &[team_f(None)]),
    ]
}

const TEAM_ALIASED_ON_MATCH: &str =
    "... on Match { team__alias_0: team { f } } ... on Race { team { f } }";

// A `Match` event holds `team__alias_0` and a `Race` event `team`, so the renamer renames the
// `Match`'s alone and every event is sent `team { f }`. The model's
// `keyAliasedUnderOneTypeConditionAndRenamedBackIsAccepted`.
#[test]
fn a_key_aliased_under_one_type_condition_and_renamed_back_is_accepted() {
    cost_checked(
        &per_case_renamer(),
        TEAM_ALIASED_ON_MATCH,
        &[],
        &team_aliased_on_match(),
        &[renamer(&["event", "team__alias_0"], "team")],
        "{ products { cost } }",
    )
    .unwrap();
}

// The first plan as the review wrote it: the client asks for `note` as `team` under
// `... on Match`, which is why the required `team` is aliased there. The model's
// `keyAliasedBesideTheClientsAliasIsAccepted`.
#[test]
fn a_key_aliased_beside_the_clients_alias_is_accepted() {
    cost_checked(
        &per_case_renamer(),
        "... on Match { team__alias_0: team { f } team__alias_1: note } ... on Race { team { f } }",
        &[renamer(&["products", "event", "team__alias_1"], "team")],
        &team_aliased_on_match(),
        &[renamer(&["event", "team__alias_0"], "team")],
        "{ products { cost event { ... on Match { team: note } } } }",
    )
    .unwrap();
}

// Without the renamer, a `Match` event is sent `team__alias_0`. The model's
// `keyAliasedUnderOneTypeConditionAndNotRenamedIsRejected`.
#[test]
fn a_key_aliased_under_one_type_condition_and_not_renamed_is_rejected() {
    assert_rejected(
        cost_checked(
            &per_case_renamer(),
            TEAM_ALIASED_ON_MATCH,
            &[],
            &team_aliased_on_match(),
            &[],
            "{ products { cost } }",
        ),
        "does not agree with the fetch's input rewrites",
    );
}

// `team` read under its own name beside `team__alias_0` under `Match`: a `Match` event holds both,
// renamed onto one key. The model's `keyAliasedBesideItsBareFieldIsRejected`, its bare `team`
// under `... on Match` here, as this `sched`'s `Event` declares no `team`.
#[test]
fn a_key_aliased_beside_its_bare_field_is_rejected() {
    let mut event = vec![on(
        "Match",
        &[field("team", None, &[field("f", None, &[])])],
    )];
    event.extend(team_aliased_on_match());
    assert_rejected(
        cost_checked(
            &per_case_renamer(),
            &format!("... on Match {{ team {{ f }} }} {TEAM_ALIASED_ON_MATCH}"),
            &[],
            &event,
            &[renamer(&["event", "team__alias_0"], "team")],
            "{ products { cost } }",
        ),
        "under one key `event.team` in one representation",
    );
}

//==================================================================================================
// Two aliases onto one key

/// `f` read under `f__alias_0` below a `Match` and under `f__alias_1` below a `race_type`.
fn f_aliased_per_type(race_type: &str) -> Vec<Value> {
    let team_f = |alias| field("team", None, &[field("f", Some(alias), &[])]);
    vec![
        on("Match", &[team_f("f__alias_0")]),
        on(race_type, &[team_f("f__alias_1")]),
    ]
}

fn rename_f_back() -> Vec<Value> {
    vec![
        renamer(&["event", "team", "f__alias_0"], "f"),
        renamer(&["event", "team", "f__alias_1"], "f"),
    ]
}

// The client's `team { f: g }` holds `f` under `... on Match`, so `f` is read under `f__alias_0`
// there and under `f__alias_1` under `... on Race`. Each event holds one alias, so each renamer
// renames the key it finds, and every event is sent `team { f }`. The model's
// `keyAliasedDifferentlyPerTypeConditionAndRenamedBackIsAccepted`.
#[test]
fn a_key_aliased_differently_per_type_condition_and_renamed_back_is_accepted() {
    cost_checked(
        &renamers_onto_one_key(),
        "... on Match { team { f__alias_0: f f: g } } ... on Race { team { f__alias_1: f } }",
        &[],
        &f_aliased_per_type("Race"),
        &rename_f_back(),
        "{ products { cost event { ... on Match { team { f: g } } } } }",
    )
    .unwrap();
}

// Both aliases read under `Match`: a `Match` event holds both, renamed onto one key. The model's
// `keyAliasedTwiceOnOneTypeConditionIsRejected`.
#[test]
fn a_key_aliased_twice_on_one_type_condition_is_rejected() {
    assert_rejected(
        cost_checked(
            &renamers_onto_one_key(),
            "... on Match { team { f__alias_0: f f__alias_1: f } }",
            &[],
            &f_aliased_per_type("Match"),
            &rename_f_back(),
            "{ products { cost } }",
        ),
        "under one key `event.team.f` in one representation",
    );
}
