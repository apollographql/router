//! What a plan returns, and the fetches writing one response value.
//!
//! Ports of the model's tests for `QueryPlan.returnedTypes`: below a field, the client is owed only
//! the object types some fetch of the plan selecting that field can return, read in the fetch's
//! subgraph. Then its `ResponseCopies` controls, on which fetches write one response value: fetches
//! writing the same key under the same parent combine there, while another alias or another list
//! is another value. Each plan is written inline as JSON.

use apollo_compiler::Name;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::Field;
use apollo_compiler::name;
use serde_json::Value;
use serde_json::json;

use super::super::returned::ReturnedTypes;
use super::CorrectnessError;
use super::Fixture;
use crate::correctness::response_shape_compare::PathConstraint;
use crate::correctness::response_shape_compare::PossibleTypes;
use crate::schema::position::ObjectTypeDefinitionPosition;

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

/// A `requires` entry on `type_condition`, reading `selections`: fields, nested sets and inline
/// fragments, written as in an operation.
fn entry(type_condition: &str, selections: &str) -> Value {
    let spaced = selections.replace('{', " { ").replace('}', " } ");
    let mut tokens = spaced.split_whitespace().peekable();
    json!({
        "kind": "InlineFragment",
        "typeCondition": type_condition,
        "selections": requires_selections(&mut tokens),
    })
}

fn requires_selections<'a>(
    tokens: &mut std::iter::Peekable<impl Iterator<Item = &'a str>>,
) -> Vec<Value> {
    let mut selections = Vec::new();
    while let Some(token) = tokens.next() {
        match token {
            "}" => break,
            "..." => {
                assert_eq!(tokens.next(), Some("on"));
                let type_condition = tokens.next().unwrap();
                assert_eq!(tokens.next(), Some("{"));
                selections.push(json!({
                    "kind": "InlineFragment",
                    "typeCondition": type_condition,
                    "selections": requires_selections(tokens),
                }));
            }
            name => {
                let mut field = json!({ "kind": "Field", "name": name });
                if tokens.peek() == Some(&"{") {
                    tokens.next();
                    field["selections"] = requires_selections(tokens).into();
                }
                selections.push(field);
            }
        }
    }
    selections
}

/// A flatten node at `path`, written as the planner prints one: `.`-separated keys, `@` for an
/// index, `|[A,B]` for a condition.
fn flatten(path: &str, node: Value) -> Value {
    let path: Vec<Value> = path
        .split('.')
        .map(|element| {
            let (key, condition) = match element.split_once('|') {
                Some((key, condition)) => {
                    let names: Vec<&str> = condition
                        .trim_start_matches('[')
                        .trim_end_matches(']')
                        .split(',')
                        .collect();
                    (key, json!(names))
                }
                None => (element, Value::Null),
            };
            if key == "@" {
                json!({ "AnyIndex": condition })
            } else {
                json!({ "Key": [key, condition] })
            }
        })
        .collect();
    json!({ "Flatten": { "path": path, "node": node } })
}

fn sequence(nodes: Vec<Value>) -> Value {
    json!({ "Sequence": { "nodes": nodes } })
}

fn parallel(nodes: Vec<Value>) -> Value {
    json!({ "Parallel": { "nodes": nodes } })
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

const INCOMPLETE: &str = "query plan does not fetch everything the operation requests";
const UNFETCHED_KEY: &str = "reads a key the plan has not fetched";
const UNSENT_DEMAND: &str = "the entries do not send what the subgraph demands";

//==================================================================================================
// What a plan returns

/// `source` declares no `B`, so `Choice` is `A` alone there; `other` declares `B` too, and that it
/// implements `I`.
fn returned_members() -> Fixture {
    Fixture::new(include_str!("../testdata/returned_members.graphql"))
}

const EVERY_VALUE: &str = "{ item { ... on I { value } } }";

fn names(types: Option<&IndexSet<Name>>) -> Vec<&str> {
    types
        .map(|types| types.iter().map(Name::as_str).collect())
        .unwrap_or_default()
}

// `source` returns only an `A` for `item`, and `value` is a scalar, so it returns no object.
#[test]
fn source_returns_a_alone() {
    let fixture = returned_members();
    let plan = fixture.written_plan(&fetch("source", EVERY_VALUE).to_string());
    let returned =
        ReturnedTypes::of_plan(&fixture.supergraph_schema, &fixture.subgraphs, &plan).unwrap();
    assert_eq!(
        names(returned.children(&name!("Query"), &name!("item"))),
        ["A"]
    );
    assert!(returned.children(&name!("A"), &name!("value")).is_none());
}

// The demand below `item` is narrowed to `A`, the one type returned there. The model writes the
// narrowing into the operation; the port hands it to the comparison as a path constraint.
#[test]
fn demand_is_narrowed_to_a_alone() {
    let fixture = returned_members();
    let plan = fixture.written_plan(&fetch("source", EVERY_VALUE).to_string());
    let returned =
        ReturnedTypes::of_plan(&fixture.supergraph_schema, &fixture.subgraphs, &plan).unwrap();
    let schema = fixture.supergraph_schema.schema();
    let item = Field::new(
        name!("item"),
        schema.type_field("Query", "item").unwrap().clone(),
    );
    let query = PossibleTypes::Restricted(
        [ObjectTypeDefinitionPosition::new(name!("Query"))]
            .into_iter()
            .collect(),
    );
    let (_, narrowed) = returned.constraint().for_field(&item, &query).unwrap();
    let PossibleTypes::Restricted(narrowed) = narrowed else {
        panic!("expected `item` to be narrowed");
    };
    let narrowed: Vec<&str> = narrowed.iter().map(|ty| ty.type_name.as_str()).collect();
    assert_eq!(narrowed, ["A"]);
}

// No fetch returns a `B` for `item`, so no `B` is owed a `value`, though the supergraph's `Choice`
// admits one.
#[test]
fn member_no_fetch_returns_is_excused() {
    let fixture = returned_members();
    check(&fixture, fetch("source", EVERY_VALUE), EVERY_VALUE).unwrap();
}

// `other` selects `item` too and can return a `B` there, whose `value` nothing fetched: one fetch
// returning a type is enough for it to be owed.
#[test]
fn member_another_fetch_returns_is_demanded() {
    let fixture = returned_members();
    let plan = parallel(vec![
        fetch("source", EVERY_VALUE),
        fetch("other", "{ item { ... on A { value } } }"),
    ]);
    expect_rejected(
        check(&fixture, plan, EVERY_VALUE),
        INCOMPLETE,
        "does not select `value` on B",
    );
}

//==================================================================================================
// A requirement through a type no fetch returns

/// `summaries` resolves a `Location`'s `summary` from its `media { id }`. `locations` serves the
/// location and its media, declaring `Photo` alone; `galleries` serves the same and declares
/// `Video` too.
fn requirement_through_interface() -> Fixture {
    Fixture::new(include_str!(
        "../testdata/requirement_through_interface.graphql"
    ))
}

/// `subgraph` fetches the location with its media's ids under `media`, then `summaries` its
/// `summary`.
fn check_summary(subgraph: &str, media: &str) -> Result<(), CorrectnessError> {
    let fixture = requirement_through_interface();
    let plan = sequence(vec![
        fetch(
            subgraph,
            &format!("{{ location {{ __typename id media {{ {media} }} }} }}"),
        ),
        flatten(
            "location",
            entity_fetch(
                "summaries",
                vec![entry("Location", "__typename id media { id }")],
                "... on Location { summary }",
            ),
        ),
    ]);
    check(&fixture, plan, "{ location { summary } }")
}

// `locations` returns no `Video` for `media`, so the `@requires` reaching through `Media` is owed
// an `id` on a `Photo` alone, which it fetched.
#[test]
fn requirement_through_type_no_fetch_returns_is_excused() {
    check_summary("locations", "... on Media { id }").unwrap();
}

// `galleries` can return a `Video`, whose `id` the plan never fetched.
#[test]
fn requirement_through_type_a_fetch_returns_is_demanded() {
    expect_rejected(
        check_summary("galleries", "... on Photo { id }"),
        UNFETCHED_KEY,
        "`id` is not read on Video",
    );
}

#[test]
fn requirement_met_at_every_returned_type_is_accepted() {
    check_summary("galleries", "... on Media { id }").unwrap();
}

// `locations` returns only `Photo` media, so an `id` fetched on a `Photo` alone meets the
// requirement through `Media`.
#[test]
fn requirement_met_at_the_one_returned_type_is_accepted() {
    check_summary("locations", "... on Photo { id }").unwrap();
}

//==================================================================================================
// Fetches writing one response value

/// `first` serves a `Parent`'s `child`, a `Child` that the subgraph `children` resolves by `id` and
/// serves `seed` and `value` for. `first` and `second` both serve the list `children`; `left` and
/// `right` resolve a `Child` and serve the field each is named after.
fn response_copies() -> Fixture {
    Fixture::new(include_str!("../testdata/response_copies.graphql"))
}

const CHILD_KEY: &str = "__typename id";
const CHILD_VALUE: &str = "{ parent { child { value } } }";

fn value_fetch() -> Value {
    entity_fetch(
        "children",
        vec![entry("Child", CHILD_KEY)],
        "... on Child { value }",
    )
}

/// The client asks `left` and `right`, two aliases of `parent`, or of the list `parents` when
/// `indexed`, for their child's `value`; the entity fetch runs under `left`, and under `right` when
/// `complete`.
fn check_aliases(indexed: bool, complete: bool) -> Result<(), CorrectnessError> {
    let fixture = response_copies();
    let holder = if indexed { "parents" } else { "parent" };
    let path = |alias: &str| {
        if indexed {
            format!("{alias}.@.child")
        } else {
            format!("{alias}.child")
        }
    };
    let mut nodes = vec![
        fetch(
            "first",
            &format!(
                "{{ left: {holder} {{ child {{ {CHILD_KEY} }} }} \
                 right: {holder} {{ child {{ {CHILD_KEY} }} }} }}"
            ),
        ),
        flatten(&path("left"), value_fetch()),
    ];
    if complete {
        nodes.push(flatten(&path("right"), value_fetch()));
    }
    check(
        &fixture,
        sequence(nodes),
        &format!(
            "{{ left: {holder} {{ child {{ value }} }} right: {holder} {{ child {{ value }} }} }}"
        ),
    )
}

// `right` holds the same child with the same key as `left`, and is still another response value:
// the router writes `left`'s `value` under `left` alone.
#[test]
fn another_alias_cannot_borrow_is_rejected() {
    expect_rejected(
        check_aliases(false, false),
        INCOMPLETE,
        "in response name: right",
    );
}

#[test]
fn each_alias_fetched_is_accepted() {
    check_aliases(false, true).unwrap();
}

// The same through lists: an element of `right` is not an element of `left`.
#[test]
fn another_list_cannot_borrow_is_rejected() {
    expect_rejected(
        check_aliases(true, false),
        INCOMPLETE,
        "in response name: right",
    );
}

#[test]
fn each_list_fetched_is_accepted() {
    check_aliases(true, true).unwrap();
}

/// Two parallel branches each fetch `parent` with its child's key; the first fetches the child's
/// `value`, and the second too when `both`.
fn check_parallel_branches(both: bool) -> Result<(), CorrectnessError> {
    let fixture = response_copies();
    let branch = |fetch_value: bool| {
        let mut nodes = vec![fetch(
            "first",
            &format!("{{ parent {{ child {{ {CHILD_KEY} }} }} }}"),
        )];
        if fetch_value {
            nodes.push(flatten("parent.child", value_fetch()));
        }
        sequence(nodes)
    };
    check(
        &fixture,
        parallel(vec![branch(true), branch(both)]),
        CHILD_VALUE,
    )
}

// Both branches write the one `parent.child`, so the router merges them and one branch's `value`
// serves it.
#[test]
fn one_branch_supplies_the_shared_child_is_accepted() {
    check_parallel_branches(false).unwrap();
}

#[test]
fn each_branch_supplying_the_child_is_accepted() {
    check_parallel_branches(true).unwrap();
}

/// An output rewrite renames `temporary` to `child`, or to `other`, beside a parallel fetch
/// writing `child { seed }`; the entity fetch then runs at `parent.child`.
fn check_renamed_child(to_child: bool) -> Result<(), CorrectnessError> {
    let fixture = response_copies();
    let mut renamed = fetch(
        "first",
        &format!("{{ parent {{ temporary: child {{ {CHILD_KEY} }} }} }}"),
    );
    renamed["Fetch"]["output_rewrites"] = json!([{ "KeyRenamer": {
        "path": [{ "Key": ["parent", null] }, { "Key": ["temporary", null] }],
        "rename_key_to": if to_child { "child" } else { "other" },
    } }]);
    let plan = sequence(vec![
        parallel(vec![
            renamed,
            fetch("first", "{ parent { child { seed } } }"),
        ]),
        flatten("parent.child", value_fetch()),
    ]);
    check(&fixture, plan, CHILD_VALUE)
}

// Renamed to `child`, the key lands in the value the entity fetch runs at.
#[test]
fn renamed_onto_the_child_merges_is_accepted() {
    check_renamed_child(true).unwrap();
}

// Renamed to `other`, the key is not under `child`, which holds only `seed`, so the router has no
// key to send.
#[test]
fn renamed_onto_another_key_cannot_borrow_is_rejected() {
    expect_rejected(
        check_renamed_child(false),
        UNFETCHED_KEY,
        "in `child`: `__typename` is not read on Child",
    );
}

/// A fetch under `@include(if: $take)` adds the child's `value`; the client asks for it under the
/// same condition, or always.
fn check_guarded(guard_demand: bool) -> Result<(), CorrectnessError> {
    let fixture = response_copies();
    let plan = parallel(vec![
        fetch(
            "first",
            &format!("{{ parent {{ child {{ {CHILD_KEY} }} }} }}"),
        ),
        json!({ "Condition": {
            "condition_variable": "take",
            "if_clause": fetch("first", "{ parent { child { value } } }"),
            "else_clause": null,
        } }),
    ]);
    let operation = if guard_demand {
        "query($take: Boolean!) { parent { child { value @include(if: $take) } } }"
    } else {
        CHILD_VALUE
    };
    check(&fixture, plan, operation)
}

#[test]
fn guarded_contribution_merges_is_accepted() {
    check_guarded(true).unwrap();
}

// Where `take` is false the router skips the fetch, and `value` is still asked.
#[test]
fn inactive_contribution_is_rejected() {
    expect_rejected(
        check_guarded(false),
        INCOMPLETE,
        "does not select `value` on Child",
    );
}

/// Two parallel root fetches of `children`, the first with `value`, the second with it when
/// `second_has_value`.
fn check_parallel_roots(second_has_value: bool) -> Result<(), CorrectnessError> {
    let fixture = response_copies();
    let second = if second_has_value {
        format!("{{ children {{ {CHILD_KEY} value }} }}")
    } else {
        format!("{{ children {{ {CHILD_KEY} }} }}")
    };
    let plan = parallel(vec![
        fetch("first", &format!("{{ children {{ {CHILD_KEY} value }} }}")),
        fetch("second", &second),
    ]);
    check(&fixture, plan, "{ children { value } }")
}

#[test]
fn both_roots_supplying_the_field_is_accepted() {
    check_parallel_roots(true).unwrap();
}

// The two roots write the same list, so the first's `value` serves every element.
#[test]
fn parallel_roots_merge_is_accepted() {
    check_parallel_roots(false).unwrap();
}

// Two entity fetches run at the elements the root fetched, and their fields combine there.
#[test]
fn entity_contributions_to_one_element_are_accepted() {
    let fixture = response_copies();
    let contribution = |subgraph: &str| {
        flatten(
            "children.@",
            entity_fetch(
                subgraph,
                vec![entry("Child", CHILD_KEY)],
                &format!("... on Child {{ {subgraph} }}"),
            ),
        )
    };
    let plan = sequence(vec![
        fetch("first", &format!("{{ children {{ {CHILD_KEY} }} }}")),
        contribution("left"),
        contribution("right"),
    ]);
    check(&fixture, plan, "{ children { left right } }").unwrap();
}

/// `root` serves an `Edge` and each vertex's `x`, `extra` resolves an `Edge` and serves `y`, and
/// `consumer` serves `result`, requiring `v { x y }`, and `span`, requiring the same of `from` and
/// `to`. The model varies one field's requirement; the fixture has a field per variant.
fn vertex_inputs() -> Fixture {
    Fixture::new(include_str!("../testdata/vertex_inputs.graphql"))
}

/// The root fetches each of `vertices`' `x`, `extra` its `y`, and `consumer`, sent both,
/// `field`.
fn check_vertex_inputs(vertices: &[&str], field: &str) -> Result<(), CorrectnessError> {
    let fixture = vertex_inputs();
    let each = |selections: &str| {
        vertices
            .iter()
            .map(|vertex| format!("{vertex} {{ {selections} }}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let plan = sequence(vec![
        fetch(
            "root",
            &format!("{{ edge {{ __typename id {} }} }}", each("x")),
        ),
        flatten(
            "edge",
            entity_fetch(
                "extra",
                vec![entry("Edge", "__typename id")],
                &format!("... on Edge {{ {} }}", each("y")),
            ),
        ),
        flatten(
            "edge",
            entity_fetch(
                "consumer",
                vec![entry("Edge", &format!("__typename id {}", each("x y")))],
                &format!("... on Edge {{ {field} }}"),
            ),
        ),
    ]);
    check(&fixture, plan, &format!("{{ edge {{ {field} }} }}"))
}

// The root's `x` and `extra`'s `y` land in one vertex, so `consumer`'s input is whole.
#[test]
fn root_and_entity_inputs_merge_is_accepted() {
    check_vertex_inputs(&["v"], "result").unwrap();
}

#[test]
fn root_and_entity_inputs_merge_at_two_vertices_is_accepted() {
    check_vertex_inputs(&["from", "to"], "span").unwrap();
}

/// `rows` serves a list of `I`, each an `A` or a `B`; `a` and `b`, each declaring one of them,
/// resolve it and serve its `datum`; `consumer` serves `result`, which requires `datum`.
fn row_contributions() -> Fixture {
    Fixture::new(include_str!("../testdata/row_contributions.graphql"))
}

/// `a` contributes the `A` rows' `datum`, and `b` the `B` rows' when `contribute_b`, before
/// `consumer` reads it for both.
fn check_rows(contribute_b: bool) -> Result<(), CorrectnessError> {
    let fixture = row_contributions();
    let contribution = |subgraph: &str, type_name: &str| {
        flatten(
            "rows.@",
            entity_fetch(
                subgraph,
                vec![entry(type_name, "__typename id")],
                &format!("... on {type_name} {{ datum }}"),
            ),
        )
    };
    let mut nodes = vec![
        fetch("rows", "{ rows { __typename id } }"),
        contribution("a", "A"),
    ];
    if contribute_b {
        nodes.push(contribution("b", "B"));
    }
    nodes.push(flatten(
        "rows.@",
        entity_fetch(
            "consumer",
            vec![
                entry("A", "__typename id datum"),
                entry("B", "__typename id datum"),
            ],
            "... on A { result } ... on B { result }",
        ),
    ));
    check(&fixture, sequence(nodes), "{ rows { result } }")
}

#[test]
fn both_contributions_supply_the_input_is_accepted() {
    check_rows(true).unwrap();
}

// `a` declaring `A` alone does not excuse the `B` rows, which `rows` returns, from the `datum`
// `consumer` reads.
#[test]
fn narrower_contribution_leaves_the_input_is_rejected() {
    expect_rejected(check_rows(false), UNFETCHED_KEY, "`datum` is not read on B");
}

/// A store's `owner` is a `Person` or a `Company`, and `profiles` resolves an owner's `Profile`
/// and serves its `locale`.
fn owner_profile() -> Fixture {
    Fixture::new(include_str!("../testdata/context_owner_profile.graphql"))
}

/// The root fetch selects the owner's `profile` bare and inside `... on Person`; the `locale` fetch
/// runs for `Person` owners only. The client asks the `locale` of a `runtime_type` owner.
fn check_mixed_copies(runtime_type: &str) -> Result<(), CorrectnessError> {
    let fixture = owner_profile();
    let plan = sequence(vec![
        fetch(
            "stores",
            "{ stores { owner { __typename profile { __typename id } \
             ... on Person { profile { __typename id } } } } }",
        ),
        flatten(
            "stores.@.owner|[Person].profile",
            entity_fetch(
                "profiles",
                vec![entry("Profile", "__typename id")],
                "... on Profile { locale }",
            ),
        ),
    ]);
    check(
        &fixture,
        plan,
        &format!("{{ stores {{ owner {{ ... on {runtime_type} {{ profile {{ locale }} }} }} }} }}"),
    )
}

// A `Person`'s profile, selected bare and under `... on Person`, is one value, and the `locale`
// fetch reaches it.
#[test]
fn guarded_copy_of_a_bare_field_is_accepted() {
    check_mixed_copies("Person").unwrap();
}

// The path's `[Person]` keeps the fetch off a `Company`'s profile, though the bare selection
// reaches it too.
#[test]
fn bare_copy_does_not_reach_an_excluded_owner_is_rejected() {
    expect_rejected(
        check_mixed_copies("Company"),
        INCOMPLETE,
        "does not select `locale` on Profile",
    );
}

//==================================================================================================
// What a field's producer can return

/// `empty` serves `thing`, a `Named`, and declares no implementation of it; `items` declares
/// `Item`. `catalog` serves `sections`, declaring `Hero` alone, and `promos` the same with `Promo`
/// too.
fn producer_types() -> Fixture {
    Fixture::new(include_str!("../testdata/producer_types.graphql"))
}

/// `empty` alone, declaring `Item`.
fn producer_types_with_item() -> Fixture {
    Fixture::new(include_str!("../testdata/producer_types_item.graphql"))
}

const THING_ID: &str = "{ thing { id } }";

// `empty` declares no implementation of `Named`, so no `thing` it returns owes an `id`.
#[test]
fn no_possible_object_result_types_is_accepted() {
    let fixture = producer_types();
    check(
        &fixture,
        fetch("empty", "{ thing { __typename } }"),
        THING_ID,
    )
    .unwrap();
}

// Declaring `Item`, `empty` can return one, which owes its `id`.
#[test]
fn added_possible_object_result_type_is_rejected() {
    let fixture = producer_types_with_item();
    expect_rejected(
        check(
            &fixture,
            fetch("empty", "{ thing { __typename } }"),
            THING_ID,
        ),
        INCOMPLETE,
        "does not select `id` on Item",
    );
}

// Returning nothing below `thing` does not excuse leaving `thing` itself out.
#[test]
fn omitted_empty_producer_field_is_rejected() {
    let fixture = producer_types();
    expect_rejected(
        check(&fixture, fetch("empty", "{ ping }"), THING_ID),
        INCOMPLETE,
        "does not select `thing` on Query",
    );
}

// A scalar has no object types, and still supplies its field.
#[test]
fn scalar_leaf_has_no_object_result_types_is_accepted() {
    let fixture = producer_types();
    check(&fixture, fetch("empty", "{ ping }"), "{ ping }").unwrap();
}

/// The client asks each section's `title` or `blurb`; `subgraph` is asked for the `title`s alone.
fn check_sections(subgraph: &str) -> Result<(), CorrectnessError> {
    let fixture = producer_types();
    check(
        &fixture,
        fetch(
            subgraph,
            "{ sections { __typename ... on Hero { title } } }",
        ),
        "{ sections { ... on Hero { title } ... on Promo { blurb } } }",
    )
}

// `catalog` returns only `Hero` sections, so no `Promo` owes a `blurb`.
#[test]
fn narrower_list_producer_is_accepted() {
    check_sections("catalog").unwrap();
}

// `promos` can return a `Promo`, which owes its `blurb`.
#[test]
fn widened_list_producer_is_rejected() {
    expect_rejected(
        check_sections("promos"),
        INCOMPLETE,
        "does not select `blurb` on Promo",
    );
}

/// The client asks every section's `name`, a field of the interface `Section`; `subgraph` is asked
/// for it on a `Hero` alone.
fn check_section_names(subgraph: &str) -> Result<(), CorrectnessError> {
    let fixture = producer_types();
    check(
        &fixture,
        fetch(subgraph, "{ sections { __typename ... on Hero { name } } }"),
        "{ sections { name } }",
    )
}

// `catalog` returns only `Hero` sections, so the interface's `name` is owed on a `Hero` alone.
#[test]
fn interface_field_fetched_on_every_returned_type_is_accepted() {
    check_section_names("catalog").unwrap();
}

// `promos` can return a `Promo`, which owes its `name` too.
#[test]
fn interface_field_missing_on_a_returned_type_is_rejected() {
    expect_rejected(
        check_section_names("promos"),
        INCOMPLETE,
        "does not select `name` on Promo",
    );
}

//==================================================================================================
// An entity case at the types a fetch returns

/// `source` and `other` share `item`, an `I`; `source` declares `A` alone, `other` `B` and its `uri`
/// too. `extra` resolves an `A`'s `link` by `refId`.
fn returned_entity_case() -> Fixture {
    Fixture::new(include_str!("../testdata/returned_entity_case.graphql"))
}

/// `subgraph` fetches each item with an `A`'s key, then `extra` its `link`, for a client asking an
/// `A`'s `link` and a `B`'s `uri`.
fn check_links(subgraph: &str) -> Result<(), CorrectnessError> {
    let fixture = returned_entity_case();
    let plan = sequence(vec![
        fetch(
            subgraph,
            "{ item { __typename ... on A { __typename refId } } }",
        ),
        flatten(
            "item",
            entity_fetch(
                "extra",
                vec![entry("A", "__typename refId")],
                "... on A { link }",
            ),
        ),
    ]);
    check(
        &fixture,
        plan,
        "{ item { ... on A { link } ... on B { uri } } }",
    )
}

// `source` never returns a `B`, so no `uri` is owed, and the entity fetch's one case on `A` covers
// every item it returns.
#[test]
fn entity_case_at_the_one_returned_type_is_accepted() {
    check_links("source").unwrap();
}

// `other` can return a `B`, whose `uri` no fetch selects.
#[test]
fn entity_case_missing_a_returned_type_is_rejected() {
    expect_rejected(
        check_links("other"),
        INCOMPLETE,
        "does not select `uri` on B",
    );
}

/// `supplier` serves a `Product` and its `price`, a `Price` it declares as `FixedPrice` alone;
/// `other` does the same declaring `RangePrice` too, and resolves a `Product`; `estimates`
/// resolves a `Product` and serves `estimate`, which requires the price at both types.
fn producer_prices() -> Fixture {
    Fixture::new(include_str!("../testdata/producer_prices.graphql"))
}

const ESTIMATE: &str = "{ product { estimate } }";

/// The `estimates` fetch, its entry reading the price at `RangePrice` when `include_range`.
fn estimate_fetch(include_range: bool) -> Value {
    let range = if include_range {
        "... on RangePrice { minValue }"
    } else {
        ""
    };
    entity_fetch(
        "estimates",
        vec![entry(
            "Product",
            &format!("__typename id price {{ ... on FixedPrice {{ value }} {range} }}"),
        )],
        "... on Product { estimate }",
    )
}

/// The product with its price from `supplier`, or from `other` when `has_range`, at `RangePrice`
/// too where it can return one and the entry does not read it; then `estimate`. The model varies
/// `supplier`'s declaration; the fixture asks `other` for the wider one.
fn check_price(has_range: bool, include_range: bool) -> Result<(), CorrectnessError> {
    let fixture = producer_prices();
    let (subgraph, range) = if !has_range {
        ("supplier", "")
    } else if include_range {
        ("other", "")
    } else {
        ("other", "... on RangePrice { __typename minValue }")
    };
    let plan = sequence(vec![
        fetch(
            subgraph,
            &format!(
                "{{ product {{ __typename id price \
                 {{ ... on FixedPrice {{ __typename value }} {range} }} }} }}"
            ),
        ),
        flatten("product", estimate_fetch(include_range)),
    ]);
    check(&fixture, plan, ESTIMATE)
}

// `supplier` returns only a `FixedPrice`, whose `value` was fetched, so the entry leaving
// `RangePrice` out still sends all the demand there can be.
#[test]
fn clipped_unreachable_requirement_is_accepted() {
    check_price(false, false).unwrap();
}

// The entry reading `RangePrice` too reads nothing there, since no price can be one.
#[test]
fn full_unreachable_requirement_is_accepted() {
    check_price(false, true).unwrap();
}

// Where the price can be a `RangePrice`, the entry leaving it out does not send its `minValue`.
#[test]
fn clipped_widened_requirement_is_rejected() {
    expect_rejected(
        check_price(true, false),
        UNSENT_DEMAND,
        "`minValue` on RangePrice",
    );
}

// The entry reading `RangePrice` needs its `minValue`, which was never fetched.
#[test]
fn full_widened_requirement_is_rejected() {
    expect_rejected(
        check_price(true, true),
        UNFETCHED_KEY,
        "`minValue` is not read on RangePrice",
    );
}

// The Router planner's plan with `other` beside `supplier`: `supplier` fetches the product, `other`
// its `price` at `FixedPrice` alone, and `estimates` gets the whole entry. The plan asks `other`,
// which can return a `RangePrice`, so the entry's `minValue` is read where it was never fetched;
// the planner takes the two sharers of `price` to agree on `supplier`'s narrower type, which
// composition only hints at. The model rejects the plan, and so does this port.
#[test]
fn plan_asking_wider_sharer_is_rejected() {
    let fixture = producer_prices();
    let plan = sequence(vec![
        fetch("supplier", "{ product { __typename id } }"),
        flatten(
            "product",
            entity_fetch(
                "other",
                vec![entry("Product", "__typename id")],
                "... on Product { price { __typename ... on FixedPrice { value } } }",
            ),
        ),
        flatten("product", estimate_fetch(true)),
    ]);
    expect_rejected(
        check(&fixture, plan, ESTIMATE),
        UNFETCHED_KEY,
        "`minValue` is not read on RangePrice",
    );
}

/// A `Parent` is an `A` or a `B`, whose `child` is a `Choice`, an `X` or a `Y`, in the supergraph
/// and in `wide`; `source` declares an `A`'s child an `X` and a `B`'s a `Y`.
fn correlated_children() -> Fixture {
    Fixture::new(include_str!("../testdata/correlated_children.graphql"))
}

/// The client asks a child of either type for `value` or `other`, crossed between `A` and `B`;
/// `source` is asked for `value` under `A`, and for `value`, or `other` unless `complete`, under
/// `B`.
fn check_correlated(complete: bool) -> Result<(), CorrectnessError> {
    let fixture = correlated_children();
    let b_child = if complete { "value" } else { "other" };
    check(
        &fixture,
        fetch(
            "source",
            &format!(
                "{{ parent {{ ... on A {{ child {{ value }} }} \
                 ... on B {{ child {{ {b_child} }} }} }} }}"
            ),
        ),
        "{ parent { \
         ... on A { child { ... on X { value } ... on Y { other } } } \
         ... on B { child { ... on Y { value } ... on X { other } } } } }",
    )
}

// `source` returns an `X` below an `A` and a `Y` below a `B`, and returned types are kept per
// parent type, so neither crossed pair owes `other`.
#[test]
fn paired_cases_complete_is_accepted() {
    check_correlated(true).unwrap();
}

// A `Y` below a `B` still owes its `value`, which the `X` below an `A` cannot supply.
#[test]
fn paired_case_cannot_borrow_is_rejected() {
    expect_rejected(
        check_correlated(false),
        INCOMPLETE,
        "does not select `value` on Y",
    );
}
