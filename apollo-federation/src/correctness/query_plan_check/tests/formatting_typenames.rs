//! The `__typename` the router formats a response by.
//!
//! Ports of the model's `FormattingTypenames` tests. The router formats a response object by its
//! `__typename` where the field's type is abstract, and by that type where the object has none
//! (`apply_selection_set`), so a client fragment on a type the field's type is no subtype of owes a
//! `__typename` in the plan. Each plan is one root fetch to `source`.

use serde_json::json;

use super::CorrectnessError;
use super::Fixture;

const INCOMPLETE: &str = "query plan does not fetch everything the operation requests";

/// `I` is an interface over `A` and `B`, and `J` one implementing it; `U` is a union of `A` and
/// `B`. `item` is an `I`, `special` a `J`, and `either` a `U`.
fn fixture() -> Fixture {
    Fixture::new(include_str!("../testdata/formatting_typenames.graphql"))
}

/// The client `operation`, fetched by one root fetch to `source` selecting `fetched`.
fn checked(operation: &str, fetched: &str) -> Result<(), CorrectnessError> {
    let fixture = fixture();
    let plan = json!({ "Fetch": {
        "subgraph_name": "source",
        "variable_usages": [],
        "operation_document": fetched,
        "operation_kind": "query",
        "input_rewrites": [],
        "output_rewrites": [],
        "context_rewrites": []
    } });
    fixture.check(&fixture.written_plan(&plan.to_string()), operation)
}

#[track_caller]
fn expect_typename_missing(result: Result<(), CorrectnessError>) {
    let error = result
        .expect_err("expected the check to reject this plan")
        .to_string();
    assert!(
        error.contains(INCOMPLETE) && error.contains("does not select `__typename`"),
        "{error}"
    );
}

// `... on A` below an `I` applies by the object's `__typename`, which the plan does not fetch, so
// the router drops `z`.
#[test]
fn fragment_without_typename_is_rejected() {
    expect_typename_missing(checked(
        "{ item { ... on A { z } } }",
        "{ item { ... on A { z } } }",
    ));
}

// With the `__typename` fetched, the router applies `... on A`.
#[test]
fn fragment_with_typename_is_accepted() {
    checked(
        "{ item { ... on A { z } } }",
        "{ item { __typename ... on A { z } } }",
    )
    .unwrap();
}

// A fragment on the field's own type applies whatever the `__typename`.
#[test]
fn fragment_on_the_fields_type_needs_no_typename() {
    checked("{ item { ... on I { x } } }", "{ item { ... on I { x } } }").unwrap();
}

// A fragment on an interface the field's interface implements applies too.
#[test]
fn fragment_on_a_super_interface_needs_no_typename() {
    checked(
        "{ special { ... on I { x } } }",
        "{ special { ... on I { x } } }",
    )
    .unwrap();
}

// A fragment on a member of a union applies only by the `__typename`.
#[test]
fn union_member_fragment_without_typename_is_rejected() {
    expect_typename_missing(checked(
        "{ either { ... on A { x } } }",
        "{ either { ... on A { x } } }",
    ));
}

// A fragment on an interface below a union is no subtype either, so it owes one too.
#[test]
fn interface_fragment_below_a_union_without_typename_is_rejected() {
    expect_typename_missing(checked(
        "{ either { ... on I { x } } }",
        "{ either { ... on I { x } } }",
    ));
}

// A fragment nested in one on the field's type still applies by the `__typename`: the router reads
// every fragment at the field's type.
#[test]
fn nested_fragment_without_typename_is_rejected() {
    expect_typename_missing(checked(
        "{ item { ... on I { ... on A { z } } } }",
        "{ item { ... on I { ... on A { z } } } }",
    ));
}

// A named fragment spread is read as the inline fragment it stands for, at the field it is spread
// under: on `A` below an `I`, it owes the `__typename`; below a `J`'s `I` fragment, nothing. Not
// in the model, whose operations have no spreads.
#[test]
fn spread_fragment_owes_what_its_inline_fragment_does() {
    let operation = "{ item { ...OnA } special { ...OnI } } \
                     fragment OnA on A { z } fragment OnI on I { x }";
    expect_typename_missing(checked(
        operation,
        "{ item { ... on A { z } } special { ... on I { x } } }",
    ));
    checked(
        operation,
        "{ item { __typename ... on A { z } } special { ... on I { x } } }",
    )
    .unwrap();
}
