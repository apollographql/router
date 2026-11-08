//! A fetch's entries, their conditions, and the representation merged from them.
//!
//! Ports of the model's tests on what an entry sends of a narrowed demand, on the input key
//! renamers an entry reads through, and of its `EntryConditions` and `Representations` tests. Most
//! check a whole plan: a root fetch supplying the inputs, then one entity fetch. The rest call the
//! definition under test directly, where the model evaluates it directly too.

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::executable::Selection;
use apollo_compiler::name;
use apollo_compiler::schema::Schema;

use super::super::representation::input_rewrites_separated;
use super::super::representation::key_guards;
use super::super::requires::Reading;
use super::super::requires::entry_selections;
use super::super::requires::same_keys;
use super::super::selections::apply_key_renamers;
use super::super::selections::type_filter_admits;
use super::Fixture;
use crate::correctness::CorrectnessError;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
use crate::query_plan::FetchNode;
use crate::query_plan::QueryPlan;
use crate::query_plan::requires_selection;
use crate::schema::ValidFederationSchema;

//==================================================================================================
// Writing plans

/// A `requires` field, read under its own name.
fn field(name: &str) -> String {
    format!(r#"{{ "kind": "Field", "name": "{name}" }}"#)
}

/// A `requires` field read under `alias`.
fn aliased(alias: &str, name: &str) -> String {
    format!(r#"{{ "kind": "Field", "alias": "{alias}", "name": "{name}" }}"#)
}

/// A `requires` field with a selection set.
fn field_with(name: &str, selections: &[String]) -> String {
    format!(
        r#"{{ "kind": "Field", "name": "{name}", "selections": [{}] }}"#,
        selections.join(", ")
    )
}

/// A `requires` inline fragment, read only where each of `included` holds, each an `@include`
/// on a variable.
fn fragment(type_condition: Option<&str>, included: &[&str], selections: &[String]) -> String {
    let conditions: Vec<(&str, &str)> = included
        .iter()
        .map(|variable| ("include", *variable))
        .collect();
    fragment_under(type_condition, &conditions, selections)
}

/// A `requires` inline fragment under `conditions`, each a directive kind (`include` or `skip`)
/// on a variable.
fn fragment_under(
    type_condition: Option<&str>,
    conditions: &[(&str, &str)],
    selections: &[String],
) -> String {
    let type_condition = type_condition
        .map(|name| format!(r#""typeCondition": "{name}", "#))
        .unwrap_or_default();
    let conditions = conditions
        .iter()
        .map(|(kind, variable)| format!(r#"{{ "kind": "{kind}", "if": "{variable}" }}"#))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"{{ "kind": "InlineFragment", {type_condition}"conditions": [{conditions}], "selections": [{}] }}"#,
        selections.join(", ")
    )
}

fn on(type_condition: &str, selections: &[String]) -> String {
    fragment(Some(type_condition), &[], selections)
}

/// An input key renamer from the key `path` reaches to `to`.
fn renamer(path: &[FetchDataPathElement], to: &str) -> String {
    format!(
        r#"{{ "KeyRenamer": {{ "path": {}, "rename_key_to": "{to}" }} }}"#,
        serde_json::to_string(path).unwrap()
    )
}

/// A value setter writing `value` at `path`.
fn value_setter(path: &[FetchDataPathElement], value: &str) -> String {
    format!(
        r#"{{ "ValueSetter": {{ "path": {}, "set_value_to": "{value}" }} }}"#,
        serde_json::to_string(path).unwrap()
    )
}

/// The value setter the planner writes to send every object `type_name` admits as `type_name`, an
/// interface's name (`compute_input_rewrites_on_key_fetch`).
fn send_as(type_name: &str) -> String {
    value_setter(&[typename_equals(type_name), key("__typename")], type_name)
}

fn key(name: &str) -> FetchDataPathElement {
    FetchDataPathElement::Key(Name::new(name).unwrap(), None)
}

fn typename_equals(type_name: &str) -> FetchDataPathElement {
    FetchDataPathElement::TypenameEquals(Name::new(type_name).unwrap())
}

/// A fetch, as the JSON plan writes it. An entity fetch has `requires`, and `operation` is then
/// what it selects under `_entities`. `variables` are the Boolean variables `operation` reads.
fn fetch_json(
    subgraph: &str,
    variables: &[&str],
    requires: &[String],
    input_rewrites: &[String],
    operation: &str,
) -> String {
    let declarations = variables
        .iter()
        .map(|variable| format!("${variable}: Boolean!"))
        .collect::<Vec<_>>();
    let operation = if requires.is_empty() {
        if declarations.is_empty() {
            operation.to_string()
        } else {
            format!("query({}) {operation}", declarations.join(", "))
        }
    } else {
        let declarations = std::iter::once("$representations: [_Any!]!".to_string())
            .chain(declarations)
            .collect::<Vec<_>>();
        format!(
            "query({}) {{ _entities(representations: $representations) {{ {operation} }} }}",
            declarations.join(", ")
        )
    };
    let variable_usages = variables
        .iter()
        .map(|variable| format!(r#""{variable}""#))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"{{
            "subgraph_name": "{subgraph}",
            "id": null,
            "variable_usages": [{variable_usages}],
            "requires": [{}],
            "operation_document": "{operation}",
            "operation_name": null,
            "operation_kind": "query",
            "input_rewrites": [{}],
            "output_rewrites": [],
            "context_rewrites": []
        }}"#,
        requires.join(", "),
        input_rewrites.join(", ")
    )
}

/// `root`, a root fetch, then `entities`, an entity fetch, at `path`.
fn root_then_entities(fixture: &Fixture, root: &str, path: &str, entities: &str) -> QueryPlan {
    fixture.written_plan(&format!(
        r#"{{ "Sequence": {{ "nodes": [
            {{ "Fetch": {root} }},
            {{ "Flatten": {{ "path": {path}, "node": {{ "Fetch": {entities} }} }} }}
        ] }} }}"#
    ))
}

#[track_caller]
fn assert_rejected(result: Result<(), CorrectnessError>, reason: &str) {
    let error = result.expect_err("expected the check to reject this plan");
    assert!(error.to_string().contains(reason), "{error}");
}

//==================================================================================================
// What an entry sends of a narrowed demand

/// `locations` serves each location's `media`, a `Photo` or a `Video`; `gallery` resolves a
/// `Location` by `id`, its `photoCount` requiring `media { ... on Photo { id } }` and its
/// `mediaCount` requiring `media { id }`.
fn gallery() -> Fixture {
    Fixture::new(include_str!("../testdata/media_requires.graphql"))
}

/// Every location's `count`, asked of `gallery` through one entry reading `media` as `media`.
fn check_media_count(count: &str, media: &[String]) -> Result<(), CorrectnessError> {
    let fixture = gallery();
    let entry = on(
        "Location",
        &[field("__typename"), field("id"), field_with("media", media)],
    );
    let plan = root_then_entities(
        &fixture,
        &fetch_json(
            "locations",
            &[],
            &[],
            &[],
            "{ locations { __typename id media { __typename id } } }",
        ),
        r#"[{ "Key": ["locations", null] }, { "AnyIndex": null }]"#,
        &fetch_json(
            "gallery",
            &[],
            &[entry],
            &[],
            &format!("... on Location {{ {count} }}"),
        ),
    );
    fixture.check(&plan, &format!("{{ locations {{ {count} }} }}"))
}

// `gallery` asks only for each photo's `id`; the entry sends every medium's `id`, a video's too,
// which the plan fetched, so sending more than the demand is fine.
#[test]
fn entry_wider_than_a_narrowed_demand_covers_it() {
    check_media_count("photoCount", &[field("id")]).unwrap();
}

// `gallery` asks for every medium's `id`, and the entry sends it only for a photo: a video is sent
// without its `id`.
#[test]
fn narrower_entry_does_not_cover_the_demand() {
    assert_rejected(
        check_media_count("mediaCount", &[on("Photo", &[field("id")])]),
        "the entries do not send what the subgraph demands",
    );
}

//==================================================================================================
// The input key renamers an entry reads through

/// `catalog` serves the feed of `Book`s and `Film`s and declares `Item` an entity interface;
/// `reviews` resolves a `Book` or a `Film` by `id`, its `blurb` requiring `isbn`; `notes` declares
/// `Item` an interface object, resolved by `id`, its `note` requiring `isbn`.
fn books() -> Fixture {
    Fixture::new(include_str!("../testdata/book_film_isbn.graphql"))
}

const FEED_PATH: &str = r#"[{ "Key": ["feed", null] }, { "AnyIndex": null }]"#;

/// The `requires` entry for `type_name`, its key and `input`.
fn entry_on(type_name: &str, input: String) -> String {
    on(type_name, &[field("__typename"), field("id"), input])
}

/// `catalog` fetching the feed, each `Book` selecting `__typename id book` and, if given, each
/// `Film` selecting `__typename id film`.
fn feed_fetch(book: &str, film: Option<&str>) -> String {
    let film = film
        .map(|film| format!(" ... on Film {{ __typename id {film} }}"))
        .unwrap_or_default();
    fetch_json(
        "catalog",
        &[],
        &[],
        &[],
        &format!("{{ feed {{ __typename ... on Book {{ __typename id {book} }}{film} }} }}"),
    )
}

/// The feed fetched by `feed`, then every `Book`'s `blurb` from `reviews` through `entry`.
fn check_book_blurbs(
    feed: &str,
    entry: String,
    input_rewrites: &[String],
) -> Result<(), CorrectnessError> {
    let fixture = books();
    let plan = root_then_entities(
        &fixture,
        &feed_fetch(feed, None),
        FEED_PATH,
        &fetch_json(
            "reviews",
            &[],
            &[entry],
            input_rewrites,
            "... on Book { blurb }",
        ),
    );
    fixture.check(&plan, "{ feed { ... on Book { blurb } } }")
}

/// The feed fetched by `book` and `film`, then every item's `blurb` from `reviews`, one entry for
/// each type.
fn check_item_blurbs(
    (book, book_input): (&str, String),
    (film, film_input): (&str, String),
    input_rewrites: &[String],
) -> Result<(), CorrectnessError> {
    let fixture = books();
    let plan = root_then_entities(
        &fixture,
        &feed_fetch(book, Some(film)),
        FEED_PATH,
        &fetch_json(
            "reviews",
            &[],
            &[entry_on("Book", book_input), entry_on("Film", film_input)],
            input_rewrites,
            "... on Book { blurb } ... on Film { blurb }",
        ),
    );
    fixture.check(
        &plan,
        "{ feed { ... on Book { blurb } ... on Film { blurb } } }",
    )
}

fn rename_isbn_back() -> String {
    renamer(&[key("__require_0_isbn")], "isbn")
}

// The renamer is from `__require_0_isbn`, which the entry does not read, so the router renames
// nothing in what it reads and sends `isbn` read under its own name. The plan fetched `isbn` only
// under the alias, so the plain `isbn` the entry reads is not there.
#[test]
fn renamer_the_entry_does_not_read_leaves_it_alone() {
    assert_rejected(
        check_book_blurbs(
            "__require_0_isbn: isbn",
            entry_on("Book", field("isbn")),
            &[rename_isbn_back()],
        ),
        "reads a key the plan has not fetched",
    );
}

// With `isbn` fetched under its own name the router reads it there, and the renamer renames
// nothing.
#[test]
fn renamer_the_entry_does_not_read_is_accepted() {
    check_book_blurbs(
        "isbn",
        entry_on("Book", field("isbn")),
        &[rename_isbn_back()],
    )
    .unwrap();
}

/// A fetch to `reviews` sent `entries`, with only `rename_isbn_back` as its input rewrites.
fn renaming_fetch(entries: &[String]) -> FetchNode {
    serde_json::from_str(&fetch_json(
        "reviews",
        &[],
        entries,
        &[rename_isbn_back()],
        "... on Book { blurb } ... on Film { blurb }",
    ))
    .unwrap()
}

// The alias and a plain `isbn` are both sent as `isbn`. A `Book` representation and a `Film` one
// each hold one of them; a `Book` representation holding both would carry two values under one
// key.
#[test]
fn alias_beside_plain_field_is_separated_only_across_types() {
    let fixture = books();
    let aliased_book = entry_on("Book", aliased("__require_0_isbn", "isbn"));
    let across_types = renaming_fetch(&[aliased_book.clone(), entry_on("Film", field("isbn"))]);
    let one_type = renaming_fetch(&[aliased_book, entry_on("Book", field("isbn"))]);
    assert!(input_rewrites_separated(&fixture.supergraph_schema, &across_types).is_ok());
    assert!(input_rewrites_separated(&fixture.supergraph_schema, &one_type).is_err());
}

// Only `Book` aliases its input, and the renamer applies to every type. No `Film` entry reads the
// alias, so a `Film` representation is sent the `isbn` it read under its own name.
#[test]
fn unscoped_renamer_shared_across_entity_types_is_accepted() {
    check_item_blurbs(
        (
            "__require_0_isbn: isbn",
            aliased("__require_0_isbn", "isbn"),
        ),
        ("isbn", field("isbn")),
        &[rename_isbn_back()],
    )
    .unwrap();
}

// The same renamer scoped to `Book`: an input renamer that renames for some types and not others
// is not one the checker can read the entries through.
#[test]
fn renamer_scoped_to_its_type_is_rejected() {
    assert_rejected(
        check_item_blurbs(
            (
                "__require_0_isbn: isbn",
                aliased("__require_0_isbn", "isbn"),
            ),
            ("isbn", field("isbn")),
            &[renamer(
                &[typename_equals("Book"), key("__require_0_isbn")],
                "isbn",
            )],
        ),
        "tests the type of what it renames",
    );
}

// `X -> isbn` on `Book` and `Y -> isbn` on `Film`: each renamer filters by type, which an input
// renamer may not.
#[test]
fn renamers_onto_one_key_for_disjoint_types_are_rejected() {
    assert_rejected(
        check_item_blurbs(
            ("X: isbn", aliased("X", "isbn")),
            ("Y: isbn", aliased("Y", "isbn")),
            &[
                renamer(&[typename_equals("Book"), key("X")], "isbn"),
                renamer(&[typename_equals("Film"), key("Y")], "isbn"),
            ],
        ),
        "tests the type of what it renames",
    );
}

// The same aliases without type filters: both rename onto `isbn`, but a `Book` representation
// holds only `X` and a `Film` one only `Y`, so each is sent one `isbn`.
#[test]
fn two_aliases_for_one_field_on_disjoint_types_are_accepted() {
    check_item_blurbs(
        ("X: isbn", aliased("X", "isbn")),
        ("Y: isbn", aliased("Y", "isbn")),
        &[renamer(&[key("X")], "isbn"), renamer(&[key("Y")], "isbn")],
    )
    .unwrap();
}

// One alias `a` for `isbn` on both types, and one renamer naming it back for both.
#[test]
fn shared_alias_across_disjoint_types_is_accepted() {
    check_item_blurbs(
        ("a: isbn", aliased("a", "isbn")),
        ("a: isbn", aliased("a", "isbn")),
        &[renamer(&[key("a")], "isbn")],
    )
    .unwrap();
}

// `a` holds `isbn` on a `Book` but `title` on a `Film`, and the one renamer sends it as `isbn`
// for both: a `Film` would be sent its title as its `isbn`. (The model's `Film` aliases `id`,
// which an operation cannot select beside a `String` under one response key.)
#[test]
fn shared_alias_for_different_fields_is_rejected() {
    assert_rejected(
        check_item_blurbs(
            ("a: isbn", aliased("a", "isbn")),
            ("a: title", aliased("a", "title")),
            &[renamer(&[key("a")], "isbn")],
        ),
        "does not agree with the fetch's input rewrites",
    );
}

// The client's `team: note` holds `team` under `... on Match`, so `sched` is asked for the
// `team { f }` that `cost` requires under `team__alias_0` there, and under `team` under
// `... on Race`. The router renames `team__alias_0` only in a representation that holds it, so
// every event is sent `team { f }`.
#[test]
fn a_key_aliased_under_one_type_condition_and_renamed_back_is_accepted() {
    let fixture = Fixture::new(include_str!("../testdata/per_case_renamer.graphql"));
    let plan = fixture.written_plan(
        r#"{ "Sequence": { "nodes": [
            { "Fetch": {
                "subgraph_name": "sched",
                "variable_usages": [],
                "operation_document": "{ products { __typename id event { __typename ... on Match { team__alias_0: team { f } team__alias_1: note } ... on Race { team { f } } } } }",
                "operation_kind": "query",
                "input_rewrites": [],
                "output_rewrites": [{ "KeyRenamer": {
                    "path": [{ "Key": ["products", null] }, { "Key": ["event", null] }, { "Key": ["team__alias_1", null] }],
                    "rename_key_to": "team"
                } }],
                "context_rewrites": []
            } },
            { "Flatten": {
                "path": [{ "Key": ["products", null] }, { "AnyIndex": null }],
                "node": { "Fetch": {
                    "subgraph_name": "req",
                    "variable_usages": [],
                    "requires": [{ "kind": "InlineFragment", "typeCondition": "Product", "selections": [
                        { "kind": "Field", "name": "__typename" },
                        { "kind": "Field", "name": "id" },
                        { "kind": "Field", "name": "event", "selections": [
                            { "kind": "InlineFragment", "typeCondition": "Match", "selections": [
                                { "kind": "Field", "alias": "team__alias_0", "name": "team", "selections": [
                                    { "kind": "Field", "name": "f" }
                                ] }
                            ] },
                            { "kind": "InlineFragment", "typeCondition": "Race", "selections": [
                                { "kind": "Field", "name": "team", "selections": [
                                    { "kind": "Field", "name": "f" }
                                ] }
                            ] }
                        ] }
                    ] }],
                    "operation_document": "query($representations: [_Any!]!) { _entities(representations: $representations) { ... on Product { cost } } }",
                    "operation_kind": "query",
                    "input_rewrites": [{ "KeyRenamer": {
                        "path": [{ "Key": ["event", null] }, { "Key": ["team__alias_0", null] }],
                        "rename_key_to": "team"
                    } }],
                    "output_rewrites": [],
                    "context_rewrites": []
                } }
            } }
        ] } }"#,
    );
    fixture
        .check(
            &plan,
            "{ products { cost event { ... on Match { team: note } } } }",
        )
        .unwrap();
}

// The client's `team { f: g }` holds `f` under `... on Match`, so `sched` is asked for the `f`
// that `cost` requires under `f__alias_0` there, and under `f__alias_1` under `... on Race`. A
// `Match` event holds only the first alias and a `Race` event only the second, so each renamer
// renames the key it finds and every event is sent `team { f }`.
#[test]
fn a_key_aliased_differently_per_type_condition_and_renamed_back_is_accepted() {
    let fixture = Fixture::new(include_str!("../testdata/renamers_onto_one_key.graphql"));
    let plan = fixture.written_plan(
        r#"{ "Sequence": { "nodes": [
            { "Fetch": {
                "subgraph_name": "sched",
                "variable_usages": [],
                "operation_document": "{ products { __typename id event { __typename ... on Match { team { f__alias_0: f f: g } } ... on Race { team { f__alias_1: f } } } } }",
                "operation_kind": "query",
                "input_rewrites": [],
                "output_rewrites": [],
                "context_rewrites": []
            } },
            { "Flatten": {
                "path": [{ "Key": ["products", null] }, { "AnyIndex": null }],
                "node": { "Fetch": {
                    "subgraph_name": "req",
                    "variable_usages": [],
                    "requires": [{ "kind": "InlineFragment", "typeCondition": "Product", "selections": [
                        { "kind": "Field", "name": "__typename" },
                        { "kind": "Field", "name": "id" },
                        { "kind": "Field", "name": "event", "selections": [
                            { "kind": "InlineFragment", "typeCondition": "Match", "selections": [
                                { "kind": "Field", "name": "team", "selections": [
                                    { "kind": "Field", "alias": "f__alias_0", "name": "f" }
                                ] }
                            ] },
                            { "kind": "InlineFragment", "typeCondition": "Race", "selections": [
                                { "kind": "Field", "name": "team", "selections": [
                                    { "kind": "Field", "alias": "f__alias_1", "name": "f" }
                                ] }
                            ] }
                        ] }
                    ] }],
                    "operation_document": "query($representations: [_Any!]!) { _entities(representations: $representations) { ... on Product { cost } } }",
                    "operation_kind": "query",
                    "input_rewrites": [
                        { "KeyRenamer": {
                            "path": [{ "Key": ["event", null] }, { "Key": ["team", null] }, { "Key": ["f__alias_0", null] }],
                            "rename_key_to": "f"
                        } },
                        { "KeyRenamer": {
                            "path": [{ "Key": ["event", null] }, { "Key": ["team", null] }, { "Key": ["f__alias_1", null] }],
                            "rename_key_to": "f"
                        } }
                    ],
                    "output_rewrites": [],
                    "context_rewrites": []
                } }
            } }
        ] } }"#,
    );
    fixture
        .check(
            &plan,
            "{ products { cost event { ... on Match { team { f: g } } } } }",
        )
        .unwrap();
}

//==================================================================================================
// An interface object's entries

/// The feed fetched by `feed`, then every `Book`'s `note` from `notes`, which knows only `Item`,
/// through `entry`, sending it as `Item` ([`send_as`]) before `input_rewrites`.
fn check_book_notes(
    feed: &str,
    entry: String,
    input_rewrites: &[String],
) -> Result<(), CorrectnessError> {
    let mut sent_as_item = vec![send_as("Item")];
    sent_as_item.extend_from_slice(input_rewrites);
    check_book_notes_rewritten(feed, entry, &sent_as_item)
}

/// [`check_book_notes`] with `input_rewrites` alone.
fn check_book_notes_rewritten(
    feed: &str,
    entry: String,
    input_rewrites: &[String],
) -> Result<(), CorrectnessError> {
    let fixture = books();
    let plan = root_then_entities(
        &fixture,
        &feed_fetch(feed, None),
        FEED_PATH,
        &fetch_json(
            "notes",
            &[],
            &[entry],
            input_rewrites,
            "... on Item { note }",
        ),
    );
    fixture.check(&plan, "{ feed { ... on Book { note } } }")
}

// `isbn` aliased for a `Book` and named back by a renamer scoped to `Book`, which an input renamer
// may not be, though the interface object's case is written at `Item`.
#[test]
fn interface_object_with_renamer_scoped_to_its_type_is_rejected() {
    assert_rejected(
        check_book_notes(
            "__require_0_isbn: isbn",
            entry_on("Book", aliased("__require_0_isbn", "isbn")),
            &[renamer(
                &[typename_equals("Book"), key("__require_0_isbn")],
                "isbn",
            )],
        ),
        "tests the type of what it renames",
    );
}

// The `Item` case is sent only `Book` representations, so it is checked at `Book` alone. No
// `Film` is sent, and nothing is compared for one: there is nothing to compare.
#[test]
fn interface_object_compares_no_empty_operation() {
    check_book_notes("isbn", entry_on("Book", field("isbn")), &[]).unwrap();
}

// The same with no value setter: a `Book` is sent as `Book`, which `notes` does not resolve.
#[test]
fn interface_object_sent_under_its_own_name_is_rejected() {
    assert_rejected(
        check_book_notes_rewritten("isbn", entry_on("Book", field("isbn")), &[]),
        "a `Book` is sent as `Book`",
    );
}

// A setter sending a `Book` as `Film` runs `Film`'s resolver on the `Book`'s `id`, which can
// resolve another entity. (The model has `notes` resolve `Film` too; the check here reads only the
// name sent, whatever the subgraph resolves.)
#[test]
fn interface_object_sent_under_another_name_is_rejected() {
    assert_rejected(
        check_book_notes_rewritten(
            "isbn",
            entry_on("Book", field("isbn")),
            &[value_setter(
                &[typename_equals("Item"), key("__typename")],
                "Film",
            )],
        ),
        "a `Book` is sent as `Film`",
    );
}

// A setter overwriting the key `id` sends every `Book` with one `id`.
#[test]
fn interface_object_with_a_key_overwritten_is_rejected() {
    assert_rejected(
        check_book_notes(
            "isbn",
            entry_on("Book", field("isbn")),
            &[value_setter(&[typename_equals("Item"), key("id")], "1")],
        ),
        "not a representation's `__typename`",
    );
}

//==================================================================================================
// An entity interface's entries

/// `catalog` serves the feed; `reviews` and `ratings` declare `Item` an entity interface, the
/// latter with its key not resolvable; `shelves` and `racks` declare `Item` a plain interface,
/// which only `shelves`'s `Book` implements.
fn entity_interfaces() -> Fixture {
    Fixture::new(include_str!("../testdata/entity_interface.graphql"))
}

/// The feed, each `Book` with its `isbn`, then `subgraph` asked `cases` for every `Book` through
/// one entry, with `input_rewrites`.
fn check_entity_interface(
    subgraph: &str,
    cases: &str,
    input_rewrites: &[String],
    operation: &str,
) -> Result<(), CorrectnessError> {
    let fixture = entity_interfaces();
    let plan = root_then_entities(
        &fixture,
        &feed_fetch("isbn", None),
        FEED_PATH,
        &fetch_json(
            subgraph,
            &[],
            &[entry_on("Book", field("isbn"))],
            input_rewrites,
            cases,
        ),
    );
    fixture.check(&plan, operation)
}

/// [`check_entity_interface`] as the planner writes a fetch to an entity interface: a `Book` case
/// beside the interface's own.
fn check_book_and_item(
    subgraph: &str,
    field: &str,
    input_rewrites: &[String],
) -> Result<(), CorrectnessError> {
    check_entity_interface(
        subgraph,
        &format!("... on Book {{ {field} }} ... on Item {{ id }}"),
        input_rewrites,
        &format!("{{ feed {{ ... on Book {{ {field} }} }} }}"),
    )
}

// The planner sends every `Book` as `Item`. `reviews` resolves it through `Item`'s reference
// resolver, which returns the `Book`, so the `Book` case applies too. The corpus's
// `kiva-marketplace` plans have this shape.
#[test]
fn implementation_case_sent_as_its_entity_interface_is_accepted() {
    check_book_and_item("reviews", "blurb", &[send_as("Item")]).unwrap();
}

// No setter: a `Book` sent as `Book` is resolved as a `Book`, which the `Item` case applies to as
// well. The planner always writes the setter here; accepted because it is sound.
#[test]
fn entity_interface_case_sent_as_the_implementation_is_accepted() {
    check_book_and_item("reviews", "blurb", &[]).unwrap();
}

// `Item`'s key is not resolvable in `ratings`, so nothing sent as `Item` is resolved there,
// whatever `Book`'s own key. The key read is the sent type's.
#[test]
fn sent_as_an_interface_without_a_resolvable_key_is_rejected() {
    assert_rejected(
        check_book_and_item("ratings", "rating", &[send_as("Item")]),
        "will not resolve the entity from this key",
    );
}

// A `Book` sent as `Film` resolves another entity, which neither case applies to.
#[test]
fn implementation_sent_as_a_sibling_is_rejected() {
    assert_rejected(
        check_book_and_item(
            "reviews",
            "blurb",
            &[value_setter(
                &[typename_equals("Book"), key("__typename")],
                "Film",
            )],
        ),
        "a `Book` is sent as `Film`, which resolves to no object the case applies to",
    );
}

/// [`check_entity_interface`] with the `Item` case alone, a `Book` sent as `Book`.
fn check_item_case_alone(subgraph: &str) -> Result<(), CorrectnessError> {
    check_entity_interface(
        subgraph,
        "... on Item { id }",
        &[],
        "{ feed { ... on Book { id } } }",
    )
}

// `shelves` resolves a `Book` sent as `Book` to a `Book`, which its `Item` case applies to.
#[test]
fn plain_interface_case_sent_an_implementation_is_accepted() {
    check_item_case_alone("shelves").unwrap();
}

// In `racks` the `Item` case applies to nothing resolved as a `Book`, though the supergraph's
// `Item` includes `Book`. The case's inclusion is read in the subgraph's schema.
#[test]
fn case_excluding_the_object_in_the_subgraph_is_rejected() {
    assert_rejected(
        check_item_case_alone("racks"),
        "a `Book` is sent as `Book`, which resolves to no object the case applies to",
    );
}

//==================================================================================================
// A renamer's type filter

// `rename_at_path` tests a `TypenameEquals` before descending past the key after it, so a renamer
// written `|Book.a.b` renames `b` below a `Book`'s `a` and not below a `Film`'s.
#[test]
fn typename_equals_filters_the_descent() {
    let schema = Schema::parse_and_validate(
        r#"
            type Query { feed: [Item] }
            interface Item { a: A }
            type Book implements Item { a: A }
            type Film implements Item { a: A }
            type A { b: Int }
        "#,
        "schema.graphql",
    )
    .unwrap();
    let schema = ValidFederationSchema::new(schema).unwrap();
    let feed = |operation: &str| -> Vec<Selection> {
        let document =
            ExecutableDocument::parse_and_validate(schema.schema(), operation, "operation.graphql")
                .unwrap();
        let Selection::Field(feed) = &document
            .operations
            .get(None)
            .unwrap()
            .selection_set
            .selections[0]
        else {
            unreachable!("the operation selects `feed`")
        };
        feed.selection_set.selections.clone()
    };
    let rewrite = serde_json::from_str::<FetchDataRewrite>(&renamer(
        &[typename_equals("Book"), key("a"), key("b")],
        "c",
    ))
    .unwrap();
    let renamed = apply_key_renamers(
        &schema,
        type_filter_admits,
        &[rewrite.into()],
        feed("{ feed { ... on Book { a { b } } ... on Film { a { b } } } }"),
    );
    assert!(same_keys(
        &renamed,
        &feed("{ feed { ... on Book { a { c: b } } ... on Film { a { b } } } }"),
    ));
}

//==================================================================================================
// An entry's conditions

/// `first` serves each `T` and its `a`, `e` and `u`; `second` resolves a `T` by `id`, its `b`
/// requiring `a`, its `d` requiring `e`, its `both` requiring `u { left right }`, and its `c`
/// requiring nothing. `T` implements `I`, which has `a`.
fn representations() -> Fixture {
    Fixture::new(include_str!("../testdata/entry_representations.graphql"))
}

fn key_inputs() -> Vec<String> {
    vec![field("__typename"), field("id")]
}

fn key_inputs_and(more: Vec<String>) -> Vec<String> {
    let mut inputs = key_inputs();
    inputs.extend(more);
    inputs
}

/// `first` fetches `fetched` under `t`, then `second` is sent `entries` and runs `cases` under
/// `_entities`, for a client asking `asked` under `t`. Any of them may read `$x` and `$y`.
fn check_t(
    fetched: &str,
    entries: &[String],
    cases: &str,
    asked: &str,
) -> Result<(), CorrectnessError> {
    let fixture = representations();
    let variables = |text: &str| {
        ["x", "y"]
            .into_iter()
            .filter(|variable| text.contains(&format!("${variable}")))
            .collect::<Vec<_>>()
    };
    let plan = root_then_entities(
        &fixture,
        &fetch_json(
            "first",
            &variables(fetched),
            &[],
            &[],
            &format!("{{ t {{ {fetched} }} }}"),
        ),
        r#"[{ "Key": ["t", null] }]"#,
        &fetch_json("second", &variables(cases), entries, &[], cases),
    );
    let declared = variables(asked)
        .iter()
        .map(|variable| format!("${variable}: Boolean!"))
        .collect::<Vec<_>>();
    let declarations = if declared.is_empty() {
        String::new()
    } else {
        format!("({})", declared.join(", "))
    };
    fixture.check(&plan, &format!("query{declarations} {{ t {{ {asked} }} }}"))
}

const KEY_AND_A: &str = "__typename id a";

// The entry is read only where `$x` holds, and `b` is asked only there: where `$x` is false the
// router sends nothing, and nothing is owed.
#[test]
fn conditioned_entry_for_a_conditioned_field_is_accepted() {
    check_t(
        KEY_AND_A,
        &[fragment(
            Some("T"),
            &["x"],
            &key_inputs_and(vec![field("a")]),
        )],
        "... on T { b }",
        "b @include(if: $x)",
    )
    .unwrap();
}

// The same entry for a `b` asked whatever `$x` is: where `$x` is false no representation is sent,
// so nothing fetches `b`.
#[test]
fn conditioned_entry_for_an_unconditioned_field_is_rejected() {
    assert_rejected(
        check_t(
            KEY_AND_A,
            &[fragment(
                Some("T"),
                &["x"],
                &key_inputs_and(vec![field("a")]),
            )],
            "... on T { b }",
            "b",
        ),
        "query plan does not fetch everything the operation requests",
    );
}

// The entry sends `a` only where `$x` holds, but `b` is selected whatever `$x` is: where it is
// false, `second` is asked for `b` without its input.
#[test]
fn input_left_out_where_its_field_is_selected_is_rejected() {
    assert_rejected(
        check_t(
            KEY_AND_A,
            &[on(
                "T",
                &key_inputs_and(vec![fragment(None, &["x"], &[field("a")])]),
            )],
            "... on T { b }",
            "b",
        ),
        "the entries do not send what the subgraph demands",
    );
}

// `a` is sent under the condition `b` is selected under, as the router writes it since
// `89df93e07b`.
#[test]
fn input_conditioned_as_its_field_is_accepted() {
    check_t(
        KEY_AND_A,
        &[on(
            "T",
            &key_inputs_and(vec![fragment(None, &["x"], &[field("a")])]),
        )],
        "... on T { b @include(if: $x) }",
        "b @include(if: $x)",
    )
    .unwrap();
}

// `a` sent whatever `$x` is, for a `b` selected only under `$x`, as the router wrote it before
// `89df93e07b`: sending more than the demand is fine, `a` being fetched.
#[test]
fn input_sent_beyond_a_conditioned_demand_is_accepted() {
    check_t(
        KEY_AND_A,
        &[on("T", &key_inputs_and(vec![field("a")]))],
        "... on T { b @include(if: $x) }",
        "b @include(if: $x)",
    )
    .unwrap();
}

// The same, with `a` fetched only under `$x`: where it is false the router reads a missing `a`.
#[test]
fn input_sent_beyond_a_conditioned_demand_unfetched_is_rejected() {
    assert_rejected(
        check_t(
            "__typename id a @include(if: $x)",
            &[on("T", &key_inputs_and(vec![field("a")]))],
            "... on T { b @include(if: $x) }",
            "b @include(if: $x)",
        ),
        "reads a key the plan has not fetched",
    );
}

// `a` sent for a `c` that requires nothing: more than the demand, and fetched.
#[test]
fn input_sent_without_a_demand_is_accepted() {
    check_t(
        KEY_AND_A,
        &[on("T", &key_inputs_and(vec![field("a")]))],
        "... on T { c }",
        "c",
    )
    .unwrap();
}

// `@skip` and `@include` are not repeatable, so an entry's two conditions are read as one fragment
// each, nested, as a fetch's conditions are.
#[test]
fn entry_conditions_are_nested_one_fragment_each() {
    let fixture = representations();
    let schema = &fixture.supergraph_schema;
    let entry: requires_selection::Selection =
        serde_json::from_str(&fragment(Some("T"), &["x", "y"], &[field("a")])).unwrap();
    let as_sent = entry_selections(schema, &name!("T"), Reading::AsSent, &[entry]).unwrap();
    let document = ExecutableDocument::parse_and_validate(
        schema.schema(),
        "query($x: Boolean!, $y: Boolean!) \
         { t { ... on T @include(if: $x) { ... @include(if: $y) { a } } } }",
        "operation.graphql",
    )
    .unwrap();
    let Selection::Field(t) = &document
        .operations
        .get(None)
        .unwrap()
        .selection_set
        .selections[0]
    else {
        unreachable!("the operation selects `t`")
    };
    assert_eq!(
        as_sent[0].to_string(),
        t.selection_set.selections[0].to_string()
    );
}

// The router's plan for `handles_multiple_conditions_on_abstract_types`: the entry's keys, and the
// `c` asked, sit under `@include` and `@skip` on one variable, which never both hold. The entry
// writes no key, so the router sends no representation, and nothing is owed or asked of a `T`.
#[test]
fn entry_writing_no_key_is_accepted() {
    let both = "... on T @include(if: $x) { ... on T @skip(if: $x) { c } }";
    check_t(
        KEY_AND_A,
        &[fragment(
            Some("T"),
            &["x"],
            &[fragment_under(Some("T"), &[("skip", "x")], &key_inputs())],
        )],
        both,
        both,
    )
    .unwrap();
}

// The entry writes its keys only where `$x` holds and `$y` does not, and `c` is asked wherever `$x`
// holds: where both hold no representation is sent, so nothing fetches `c`.
#[test]
fn field_asked_where_the_entry_writes_no_key_is_rejected() {
    assert_rejected(
        check_t(
            KEY_AND_A,
            &[fragment(
                Some("T"),
                &["x"],
                &[fragment_under(None, &[("skip", "y")], &key_inputs())],
            )],
            "... on T { c }",
            "c @include(if: $x)",
        ),
        "query plan does not fetch everything the operation requests",
    );
}

// The same entry with its keys on its own fragment writes them wherever `$x` holds, which is where
// `c` is asked.
#[test]
fn entry_writing_its_keys_on_its_own_fragment_is_accepted() {
    check_t(
        KEY_AND_A,
        &[fragment(
            Some("T"),
            &["x"],
            &key_inputs_and(vec![fragment_under(None, &[("skip", "y")], &[field("a")])]),
        )],
        "... on T { c }",
        "c @include(if: $x)",
    )
    .unwrap();
}

// A fragment holding a field writes wherever it is entered, so a chain stops there; one holding
// only fragments continues into each.
#[test]
fn key_guards_stop_at_a_fragment_holding_a_field() {
    let chains = |entry: String| -> Vec<Vec<(Option<String>, usize)>> {
        let entry: requires_selection::Selection = serde_json::from_str(&entry).unwrap();
        key_guards(&entry)
            .into_iter()
            .map(|chain| {
                chain
                    .into_iter()
                    .map(|fragment| {
                        (
                            fragment
                                .type_condition
                                .as_ref()
                                .map(|name| name.to_string()),
                            fragment.conditions.len(),
                        )
                    })
                    .collect()
            })
            .collect()
    };
    assert_eq!(
        chains(fragment(
            Some("T"),
            &["x"],
            &key_inputs_and(vec![fragment_under(None, &[("skip", "y")], &[field("a")])]),
        )),
        vec![vec![(Some("T".to_string()), 1)]],
    );
    assert_eq!(
        chains(fragment(
            Some("T"),
            &["x"],
            &[fragment_under(Some("T"), &[("skip", "x")], &key_inputs())],
        )),
        vec![vec![(Some("T".to_string()), 1), (Some("T".to_string()), 1)]],
    );
}

//==================================================================================================
// One representation per object, out of every entry that applies to it

const KEY_FIELDS: &str = "__typename id";

// The key in one entry and `a` in another: the router merges both into the one representation it
// sends, which then has what `b` needs, though neither entry alone does.
#[test]
fn representation_split_across_entries_is_accepted() {
    check_t(
        KEY_AND_A,
        &[on("T", &key_inputs()), on("T", &[field("a")])],
        "... on T { b }",
        "b",
    )
    .unwrap();
}

// The router's shape since `89df93e07b`: the key unconditionally, and `a` in an entry of its own
// under the condition `b` is selected under.
#[test]
fn conditioned_input_entry_is_accepted() {
    check_t(
        KEY_AND_A,
        &[
            on("T", &key_inputs()),
            fragment(Some("T"), &["x"], &[field("a")]),
        ],
        "... on T { b @include(if: $x) }",
        "b @include(if: $x)",
    )
    .unwrap();
}

// The same entries for a `b` selected whatever `$x` is: where it is false, the representation has
// no `a`.
#[test]
fn conditioned_input_entry_for_an_unconditioned_field_is_rejected() {
    assert_rejected(
        check_t(
            KEY_AND_A,
            &[
                on("T", &key_inputs()),
                fragment(Some("T"), &["x"], &[field("a")]),
            ],
            "... on T { b }",
            "b",
        ),
        "the entries do not send what the subgraph demands",
    );
}

// An entry on `I` is merged into a `T`'s representation beside the one on `T`.
#[test]
fn entries_on_an_interface_and_its_implementation_are_accepted() {
    check_t(
        KEY_AND_A,
        &[on("T", &key_inputs()), on("I", &[field("a")])],
        "... on T { b }",
        "b",
    )
    .unwrap();
}

// Two `_entities` fragments on `T` are one entity case, demanding `a` for `b` and `e` for `d` of
// one representation, which the one entry carries.
#[test]
fn case_fragments_on_one_type_are_read_together() {
    check_t(
        "__typename id a e",
        &[on("T", &key_inputs_and(vec![field("a"), field("e")]))],
        "... on T { b } ... on T { d }",
        "b d",
    )
    .unwrap();
}

// The same, with `e` missing from the entry: `second` is asked for `d` without its input.
#[test]
fn input_missing_for_one_case_fragment_is_rejected() {
    assert_rejected(
        check_t(
            "__typename id a e",
            &[on("T", &key_inputs_and(vec![field("a")]))],
            "... on T { b } ... on T { d }",
            "b d",
        ),
        "the entries do not send what the subgraph demands",
    );
}

// A fragment excluded outright, by `@include(if: false)`, is never run, so its `b` demands no `a`.
#[test]
fn case_fragment_excluded_outright_demands_nothing() {
    check_t(
        KEY_FIELDS,
        &[on("T", &key_inputs())],
        "... on T { c } ... on T @include(if: false) { b }",
        "c",
    )
    .unwrap();
}

/// `second` asked for `both`, whose `u { left right }` the entry reads as `selections`.
fn check_both(selections: Vec<String>) -> Result<(), CorrectnessError> {
    check_t(
        "__typename id u { left right }",
        &[on("T", &key_inputs_and(selections))],
        "... on T { both }",
        "both",
    )
}

// `u { right }` is read inside a fragment, then `u { left }` as a field after it, which replaces
// the value the fragment put under `u`: `second` is sent no `right`.
#[test]
fn field_overwriting_what_a_fragment_read_is_rejected() {
    assert_rejected(
        check_both(vec![
            on("T", &[field_with("u", &[field("right")])]),
            field_with("u", &[field("left")]),
        ]),
        "overwrites `u`, which it read before",
    );
}

// The other order: the fragment's `u` is deep-merged into the field's, and both are sent.
#[test]
fn fragment_merged_into_a_field_is_accepted() {
    check_both(vec![
        field_with("u", &[field("left")]),
        on("T", &[field_with("u", &[field("right")])]),
    ])
    .unwrap();
}
