//! What a flatten path's type conditions select, and the `__typename` they test.
//!
//! The router's `iterate_path` follows a conditioned path element into an object only when the
//! object's `__typename` is literally one of the condition's names, so a name that is no object
//! type selects nothing, and an object whose `__typename` was never fetched is skipped. A key's
//! condition is compared with the value the key holds, so one on a key holding a list selects
//! nothing below it.

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::executable;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::name;
use apollo_compiler::schema::Schema;

use super::super::selections::selections_at;
use super::super::selections::under_condition;
use super::Fixture;
use crate::correctness::CheckerOptions;
use crate::correctness::CorrectnessError;
use crate::correctness::query_compare::conditions::BooleanLiteral;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::QueryPlan;
use crate::schema::ValidFederationSchema;

//==================================================================================================
// Mounting under a path condition

/// `t` is an `I`, which is an `A` or a `B`.
const SCHEMA: &str = r#"
    type Query { t(sort: String): I! }
    interface I { prop: String, u: U }
    type A implements I { prop: String, u: U }
    type B implements I { prop: String, u: U }
    type U { id: ID, name: String }
"#;

fn schema() -> ValidFederationSchema {
    let schema = Schema::parse_and_validate(SCHEMA, "schema.graphql").unwrap();
    ValidFederationSchema::new(schema).unwrap()
}

/// The root selections of `source`, an operation on `schema`.
fn selections(schema: &ValidFederationSchema, source: &str) -> Vec<Selection> {
    let document =
        ExecutableDocument::parse_and_validate(schema.schema(), source, "operation.graphql")
            .unwrap();
    document
        .operations
        .get(None)
        .unwrap()
        .selection_set
        .selections
        .clone()
}

fn print(selections: Vec<Selection>) -> String {
    let mut set = SelectionSet::new(name!("Query"));
    set.selections = selections;
    set.to_string()
}

fn key(name: &str, condition: Option<&[&str]>) -> FetchDataPathElement {
    FetchDataPathElement::Key(Name::new(name).unwrap(), names(condition))
}

fn index(condition: Option<&[&str]>) -> FetchDataPathElement {
    FetchDataPathElement::AnyIndex(names(condition))
}

fn names(condition: Option<&[&str]>) -> Option<Vec<Name>> {
    condition.map(|names| names.iter().map(|name| Name::new(name).unwrap()).collect())
}

// `@skip` and `@include` are not repeatable, so a fetch under two condition nodes is guarded by
// one fragment per condition, nested as the planner nests them; one fragment carrying both would
// be an invalid operation.
#[test]
fn fetch_conditions_are_nested_one_fragment_each() {
    let schema = schema();
    let guarded = under_condition(
        &[
            BooleanLiteral::Positive(name!("x")),
            BooleanLiteral::Positive(name!("y")),
        ],
        &name!("Query"),
        selections(&schema, "{ t { prop } }"),
    );
    assert_eq!(
        print(guarded),
        print(selections(
            &schema,
            "query($x: Boolean!, $y: Boolean!) \
             { ... @include(if: $x) { ... @include(if: $y) { t { prop } } } }",
        )),
    );
}

// A fetch serving both of an interface's members runs at `t|[A,B].u`, and what the plan fetched
// at `t` has a fragment on each. Each fragment already states the narrowing, so `u` is mounted
// once under each, not again under every name of the condition (two copies per fragment, one
// under a name contradicting the fragment). Mounting a second fetch at the same path into those
// copies finds them again one per fragment, so a chain of fetches is not multiplied either.
#[test]
fn field_under_a_fragment_on_a_narrowed_type_is_mounted_once() {
    let schema = schema();
    let path = [key("t", Some(&["A", "B"])), key("u", None)];
    let id: Selection = executable::Field::new(
        name!("id"),
        schema.schema().type_field("U", "id").unwrap().clone(),
    )
    .into();
    let copies_per_fragment = "{ t { ... on A { u { id } } ... on B { u { id } } } }";

    let available = selections(
        &schema,
        "{ t { ... on A { prop u { id } } ... on B { prop u { id } } } }",
    );
    assert_eq!(
        print(selections_at(
            &schema,
            &available,
            None,
            &path,
            std::slice::from_ref(&id)
        )),
        print(selections(&schema, copies_per_fragment)),
    );

    let available = selections(&schema, copies_per_fragment);
    assert_eq!(
        print(selections_at(
            &schema,
            &available,
            None,
            &path,
            std::slice::from_ref(&id)
        )),
        print(selections(&schema, copies_per_fragment)),
    );
}

/// `U.name`, which a fetch at `t|...|.u` mounts.
fn u_name(schema: &ValidFederationSchema) -> Selection {
    executable::Field::new(
        name!("name"),
        schema.schema().type_field("U", "name").unwrap().clone(),
    )
    .into()
}

// Upstream's `a_bare_field_under_a_wider_fragment_is_narrowed`.
// Under a fragment on `I`, which also reaches the `B` the path `t|[A].u` excludes, the fragment
// states nothing, so `u` found inside it is guarded by `A` as it is without one.
#[test]
fn field_under_a_fragment_wider_than_the_condition_is_narrowed() {
    let schema = schema();
    let path = [key("t", Some(&["A"])), key("u", None)];
    let available = selections(&schema, "{ t { ... on I { u { id } } } }");
    assert_eq!(
        print(selections_at(
            &schema,
            &available,
            None,
            &path,
            &[u_name(&schema)]
        )),
        print(selections(
            &schema,
            "{ t { ... on I { ... on A { u { name } } } } }"
        )),
    );
}

/// Six copies of `t` selecting `inner` at `u`: under a fragment on each member and under one on
/// `I`; each of those again under an `@include`, on the field and on its fragment; and the first
/// with a `sort` argument, given two values. Each operation is parsed alone, as each fetch's own
/// is: copies of one response key that differ in an argument cannot share an operation.
fn t_copies(schema: &ValidFederationSchema, inner: &str) -> Vec<Selection> {
    let members = format!("... on A {{ u {{ {inner} }} }} ... on B {{ u {{ {inner} }} }}");
    let any = format!("u {{ {inner} }}");
    [
        format!("{{ t {{ {members} }} t {{ ... on I {{ {any} }} }} }}"),
        format!(
            "query($v: Boolean!) {{ t @include(if: $v) {{ {members} }} \
             t {{ ... on I @include(if: $v) {{ {any} }} }} }}"
        ),
        format!("{{ t(sort: \"title\") {{ {members} }} }}"),
        format!("{{ t(sort: \"author\") {{ {members} }} }}"),
    ]
    .iter()
    .flat_map(|operation| selections(schema, operation))
    .collect()
}

// Upstream's `a_chain_of_fetches_at_one_site_is_reproduced_once_per_distinct_copy`.
// Three fetches are mounted at `t|[A,B].u` in turn, each into everything fetched before it. Each
// leaves its own copy of `t`, which a later one reproduces as it reproduces the copy it was
// mounted through, so a copy repeating one already reproduced is kept once: every mount yields
// the six copies, each differing from another in a guard, a directive or an argument alone.
// Keeping every copy, the second mount yields each twice and the third four times, doubling
// along the chain.
#[test]
fn chain_of_fetches_at_one_site_mounts_each_distinct_copy_once() {
    let schema = schema();
    let path = [key("t", Some(&["A", "B"])), key("u", None)];
    let mounted = [u_name(&schema)];
    let expected = print(t_copies(&schema, "name"));

    let mut buffer = t_copies(&schema, "id");
    let mut mounts = Vec::new();
    for _ in 0..3 {
        let reproduced = selections_at(&schema, &buffer, None, &path, &mounted);
        mounts.push(print(reproduced.clone()));
        buffer.extend(reproduced);
    }
    assert_eq!(mounts, vec![expected; 3]);
}

//==================================================================================================
// A path condition's names

fn catalog() -> Fixture {
    Fixture::new(include_str!("../testdata/path_condition_catalog.graphql"))
}

/// A fetch's fields, as the JSON plan writes them after `subgraph_name`.
const FETCH_TAIL: &str = r#""variable_usages": [],
    "operation_kind": "query",
    "input_rewrites": [],
    "output_rewrites": [],
    "context_rewrites": []"#;

fn entities(cases: &str) -> String {
    format!(
        "query($representations: [_Any!]!) \
         {{ _entities(representations: $representations) {{ {cases} }} }}"
    )
}

/// One `requires` entry per type in `entity_types`, each its `__typename` and `id`.
fn requires_by_id(entity_types: &[&str]) -> String {
    let entries = entity_types
        .iter()
        .map(|entity_type| {
            format!(
                r#"{{ "kind": "InlineFragment", "typeCondition": "{entity_type}", "selections": [
                    {{ "kind": "Field", "name": "__typename" }},
                    {{ "kind": "Field", "name": "id" }}
                ] }}"#
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{entries}]")
}

/// A root fetch to `root_subgraph` selecting `root`, then an entity fetch to `entity_subgraph` at
/// `path`, asking each of `entity_types` for `selections` and resolving it by `id`.
fn root_then_entities(
    fixture: &Fixture,
    root_subgraph: &str,
    root: &str,
    path: &[FetchDataPathElement],
    entity_subgraph: &str,
    entity_types: &[&str],
    selections: &str,
) -> QueryPlan {
    let path = serde_json::to_string(path).unwrap();
    let requires = requires_by_id(entity_types);
    let cases = entity_types
        .iter()
        .map(|entity_type| format!("... on {entity_type} {{ {selections} }}"))
        .collect::<Vec<_>>()
        .join(" ");
    let operation = entities(&cases);
    fixture.written_plan(&format!(
        r#"{{ "Sequence": {{ "nodes": [
            {{ "Fetch": {{
                "subgraph_name": "{root_subgraph}",
                "operation_document": "{root}",
                {FETCH_TAIL}
            }} }},
            {{ "Flatten": {{
                "path": {path},
                "node": {{ "Fetch": {{
                    "subgraph_name": "{entity_subgraph}",
                    "requires": {requires},
                    "operation_document": "{operation}",
                    {FETCH_TAIL}
                }} }}
            }} }}
        ] }} }}"#
    ))
}

/// An item's key, inside a fragment on each of its types.
const ITEM_KEYS: &str = "__typename ... on Book { __typename id } ... on Film { __typename id }";

const FEATURED_AUTHOR_NAME: &str = "{ featured { author { name } } }";
const FEATURED_BLURB: &str = "{ featured { ... on Book { blurb } ... on Film { blurb } } }";
const ITEMS_BLURB: &str = "{ items { ... on Book { blurb } ... on Film { blurb } } }";

/// `featured|condition.author`: the featured item's author's `name`, the condition on a key with
/// more path below it, and the root fetch selecting `author` on the interface, with no fragment
/// of its own.
fn check_featured_author(condition: &[&str]) -> Result<(), CorrectnessError> {
    let fixture = catalog();
    let plan = root_then_entities(
        &fixture,
        "catalog",
        "{ featured { __typename author { __typename id } } }",
        &[key("featured", Some(condition)), key("author", None)],
        "reviews",
        &["Author"],
        "name",
    );
    fixture.check(&plan, FEATURED_AUTHOR_NAME)
}

/// `featured|condition`: the featured item's `blurb`, the condition on the key ending the path.
fn check_featured_blurb(condition: &[&str]) -> Result<(), CorrectnessError> {
    check_featured_blurb_with(condition, CheckerOptions::default())
}

fn check_featured_blurb_with(
    condition: &[&str],
    options: CheckerOptions,
) -> Result<(), CorrectnessError> {
    let fixture = catalog();
    let plan = root_then_entities(
        &fixture,
        "catalog",
        &format!("{{ featured {{ {ITEM_KEYS} }} }}"),
        &[key("featured", Some(condition))],
        "reviews",
        &["Book", "Film"],
        "blurb",
    );
    crate::correctness::check_plan_with_options(
        &fixture.api_schema,
        &fixture.supergraph_schema,
        &fixture.subgraphs,
        &fixture.operation(FEATURED_BLURB),
        &plan,
        options,
    )
}

/// Every item's `blurb`, fetched at `path` below `items`.
fn check_items_blurb_with(
    path: &[FetchDataPathElement],
    options: CheckerOptions,
) -> Result<(), CorrectnessError> {
    let fixture = catalog();
    let plan = root_then_entities(
        &fixture,
        "catalog",
        &format!("{{ items {{ {ITEM_KEYS} }} }}"),
        path,
        "reviews",
        &["Book", "Film"],
        "blurb",
    );
    crate::correctness::check_plan_with_options(
        &fixture.api_schema,
        &fixture.supergraph_schema,
        &fixture.subgraphs,
        &fixture.operation(ITEMS_BLURB),
        &plan,
        options,
    )
}

/// `items.@|condition`: every item's `blurb`, the condition on the index ending the path.
fn check_items_blurb(condition: &[&str]) -> Result<(), CorrectnessError> {
    check_items_blurb_with(
        &[key("items", None), index(Some(condition))],
        CheckerOptions::default(),
    )
}

fn expect_rejected(result: Result<(), CorrectnessError>, expected: &str) {
    let error = result.expect_err("expected the check to reject this plan");
    assert!(error.to_string().contains(expected), "{error}");
}

// No item's `__typename` is `Item`, so `iterate_path` follows the condition into none of them and
// the entity fetch runs nowhere -- whether the condition is on a key with more path below it, on
// a key ending the path, or on an index ending it -- and the plan never fetches what is asked.
#[test]
fn fetch_under_an_interface_name_is_rejected() {
    expect_rejected(check_featured_author(&["Item"]), "does not select `name`");
    expect_rejected(check_featured_blurb(&["Item"]), "does not select `blurb`");
    expect_rejected(check_items_blurb(&["Item"]), "does not select `blurb`");
}

// The same three conditions naming the interface's members, as a planner writes them: every item
// is one of them, so the fetch runs at every item.
#[test]
fn fetch_under_member_names_is_accepted() {
    check_featured_author(&["Book", "Film"]).unwrap();
    check_featured_blurb(&["Book", "Film"]).unwrap();
    check_items_blurb(&["Book", "Film"]).unwrap();
}

// `Person` is an object type, but no featured item is one, so the fetch below `featured|[Person]`
// runs nowhere. The author's `name` is mounted under `... on Person` inside `featured`, which no
// item satisfies.
#[test]
fn fetch_under_an_object_type_the_level_cannot_hold_is_rejected() {
    expect_rejected(check_featured_author(&["Person"]), "does not select `name`");
}

/// `items|condition.@`: every item's `blurb`, the condition on the key before an unrestricted
/// index.
fn check_items_key_blurb(
    condition: &[&str],
    options: CheckerOptions,
) -> Result<(), CorrectnessError> {
    check_items_blurb_with(&[key("items", Some(condition)), index(None)], options)
}

// `iterate_path` reads a key's condition off the `__typename` of the value the key holds, and
// `items` holds a list, which has none: whatever the condition names -- the interface, nothing,
// or every member -- nothing below it is selected and the items' `blurb` is never fetched. The
// empty condition is checked with the FED-516 report off, so that it is the path, not that
// report, that rejects it.
#[test]
fn fetch_under_a_key_condition_before_an_index_is_rejected() {
    let without_empty_check = || CheckerOptions {
        check_empty_flatten_path_type_condition: false,
        ..Default::default()
    };
    expect_rejected(
        check_items_key_blurb(&["Item"], CheckerOptions::default()),
        "does not select `blurb`",
    );
    expect_rejected(
        check_items_key_blurb(&[], without_empty_check()),
        "does not select `blurb`",
    );
    expect_rejected(
        check_items_key_blurb(&["Book", "Film"], CheckerOptions::default()),
        "does not select `blurb`",
    );
}

// Upstream's `a_fetch_at_an_empty_condition_ending_its_path_is_rejected`.
// An empty condition ending the path, `featured|[]` or `items.@|[]`, names no object, so the fetch
// below it runs nowhere and the plan never fetches the `blurb`s. With the FED-516 report off, it is
// the end of the path, not that report, that rejects it.
#[test]
fn fetch_at_an_empty_condition_ending_its_path_is_rejected() {
    let without_empty_check = || CheckerOptions {
        check_empty_flatten_path_type_condition: false,
        ..Default::default()
    };
    expect_rejected(
        check_featured_blurb_with(&[], without_empty_check()),
        "does not select `blurb`",
    );
    expect_rejected(
        check_items_blurb_with(
            &[key("items", None), index(Some(&[]))],
            without_empty_check(),
        ),
        "does not select `blurb`",
    );
}

// The same `blurb`, its condition on the index instead, where `iterate_path` tests each element:
// every item satisfies it.
#[test]
fn fetch_under_an_index_condition_is_accepted() {
    check_items_blurb_with(
        &[key("items", None), index(Some(&["Book", "Film"]))],
        CheckerOptions::default(),
    )
    .unwrap();
}

/// `catalog` fetches the items' keys, then `reviews` their `blurb` twice: at `items.@`, which
/// reaches every item, and at `items.@|[]`, which reaches none.
fn check_live_and_dead_blurb(options: CheckerOptions) -> Result<(), CorrectnessError> {
    let fixture = catalog();
    let requires = requires_by_id(&["Book", "Film"]);
    let operation = entities("... on Book { blurb } ... on Film { blurb }");
    let blurb_at = |path: &[FetchDataPathElement]| {
        format!(
            r#"{{ "Flatten": {{
                "path": {},
                "node": {{ "Fetch": {{
                    "subgraph_name": "reviews",
                    "requires": {requires},
                    "operation_document": "{operation}",
                    {FETCH_TAIL}
                }} }}
            }} }}"#,
            serde_json::to_string(path).unwrap()
        )
    };
    let plan = fixture.written_plan(&format!(
        r#"{{ "Sequence": {{ "nodes": [
            {{ "Fetch": {{
                "subgraph_name": "catalog",
                "operation_document": "{{ items {{ {ITEM_KEYS} }} }}",
                {FETCH_TAIL}
            }} }},
            {},
            {}
        ] }} }}"#,
        blurb_at(&[key("items", None), index(None)]),
        blurb_at(&[key("items", None), index(Some(&[]))]),
    ));
    crate::correctness::check_plan_with_options(
        &fixture.api_schema,
        &fixture.supergraph_schema,
        &fixture.subgraphs,
        &fixture.operation(ITEMS_BLURB),
        &plan,
        options,
    )
}

// FED-516: the planner wrote a fetch under `…|[]`, which never runs. The plan is otherwise
// complete, so it is the report alone that rejects it.
#[test]
fn dead_fetch_beside_a_live_one_is_reported() {
    expect_rejected(
        check_live_and_dead_blurb(CheckerOptions::default()),
        "no runtime type",
    );
}

// With the report off the dead fetch is harmless: the live one fetches every `blurb`.
#[test]
fn dead_fetch_beside_a_live_one_is_accepted_without_the_report() {
    check_live_and_dead_blurb(CheckerOptions {
        check_empty_flatten_path_type_condition: false,
        ..Default::default()
    })
    .unwrap();
}

//==================================================================================================
// The `__typename` a path condition tests

fn parents() -> Fixture {
    Fixture::new(include_str!("../testdata/path_condition_parents.graphql"))
}

const CHILD_VALUE: &str = "{ parent { child { value } } }";

/// The parents from `first`, with their `__typename` when `observed`, and then every child's
/// `value` from `children`, at a path that tests the parent's type: `parents.@|[Parent].child`
/// when `indexed`, `parent|[Parent].child` otherwise.
fn check_value(indexed: bool, observed: bool) -> Result<(), CorrectnessError> {
    let fixture = parents();
    let holder = if indexed { "parents" } else { "parent" };
    let path = if indexed {
        vec![
            key("parents", None),
            index(Some(&["Parent"])),
            key("child", None),
        ]
    } else {
        vec![key("parent", Some(&["Parent"])), key("child", None)]
    };
    let typename = if observed { "__typename " } else { "" };
    let plan = root_then_entities(
        &fixture,
        "first",
        &format!("{{ {holder} {{ {typename}child {{ __typename id }} }} }}"),
        &path,
        "children",
        &["Child"],
        "value",
    );
    fixture.check(&plan, &format!("{{ {holder} {{ child {{ value }} }} }}"))
}

// The parent's `__typename` was never fetched, so `iterate_path` follows `parent|[Parent]` into
// no parent and the fetch runs nowhere, though the child's key is there.
#[test]
fn untestable_parent_condition_is_rejected() {
    expect_rejected(
        check_value(false, false),
        "tests `__typename` at `parent|[Parent]`",
    );
}

#[test]
fn tested_parent_condition_is_accepted() {
    check_value(false, true).unwrap();
}

// Through a list the test is on each element, whose `__typename` was never fetched.
#[test]
fn untestable_index_condition_is_rejected() {
    expect_rejected(
        check_value(true, false),
        "tests `__typename` at `parents.@|[Parent]`",
    );
}

#[test]
fn tested_index_condition_is_accepted() {
    check_value(true, true).unwrap();
}

// The root fetch already fetches the child's `value`, so the operation is complete without the
// entity fetch; but the fetch is still in the plan and the router still runs it, so it still owes
// the `__typename` its path tests.
#[test]
fn fetched_value_does_not_excuse_the_test() {
    let fixture = parents();
    let plan = root_then_entities(
        &fixture,
        "first",
        "{ parent { child { __typename id value } } }",
        &[key("parent", Some(&["Parent"])), key("child", None)],
        "children",
        &["Child"],
        "value",
    );
    expect_rejected(
        fixture.check(&plan, CHILD_VALUE),
        "tests `__typename` at `parent|[Parent]`",
    );
}

// With the parent's `__typename` fetched too, the entity fetch's test is met, and the plan is
// complete without it.
#[test]
fn fetched_value_with_the_test_is_accepted() {
    let fixture = parents();
    let plan = root_then_entities(
        &fixture,
        "first",
        "{ parent { __typename child { __typename id value } } }",
        &[key("parent", Some(&["Parent"])), key("child", None)],
        "children",
        &["Child"],
        "value",
    );
    fixture.check(&plan, CHILD_VALUE).unwrap();
}
