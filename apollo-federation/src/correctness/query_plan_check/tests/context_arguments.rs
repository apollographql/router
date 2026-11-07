//! Contextual arguments: bound by an entity fetch, passed by no other, and read from their
//! declaration; and a contextual value read off the object the fetch runs on.
//!
//! Ports of the model's `ContextArguments` and `OwnPosition` tests. Each plan is written inline:
//! one fetch serving the hierarchy a context is read from, then one fetch passing a contextual
//! argument, with the arguments and rewrites a case varies. Every verdict is the whole checker's.

use serde_json::Value;
use serde_json::json;

use super::CorrectnessError;
use super::Fixture;

//==================================================================================================
// Writing plans

/// A fetch to `subgraph` with no `requires`, selecting `operation`, with `rewrites` beside it.
fn root_fetch(subgraph: &str, operation: &str, rewrites: Vec<Value>) -> Value {
    json!({ "Fetch": {
        "subgraph_name": subgraph,
        "variable_usages": [],
        "operation_document": operation,
        "operation_kind": "query",
        "input_rewrites": [],
        "output_rewrites": [],
        "context_rewrites": rewrites
    } })
}

/// An entity fetch to `subgraph`, its `requires` written at `entry_types`, selecting `cases` under
/// `_entities`. `variables` are the `String` variables `cases` passes, which `rewrites` bind.
fn entity_fetch(
    subgraph: &str,
    entry_types: &[&str],
    cases: &str,
    variables: &[&str],
    rewrites: Vec<Value>,
) -> Value {
    let declarations: String = variables
        .iter()
        .map(|variable| format!(", ${variable}: String"))
        .collect();
    let requires: Vec<Value> = entry_types
        .iter()
        .map(|type_name| {
            json!({
                "kind": "InlineFragment",
                "typeCondition": type_name,
                "selections": [
                    { "kind": "Field", "name": "__typename" },
                    { "kind": "Field", "name": "id" }
                ]
            })
        })
        .collect();
    json!({ "Fetch": {
        "subgraph_name": subgraph,
        "variable_usages": variables,
        "requires": requires,
        "operation_document": format!(
            "query($representations: [_Any!]!{declarations}) \
             {{ _entities(representations: $representations) {{ {cases} }} }}"
        ),
        "operation_kind": "query",
        "input_rewrites": [],
        "output_rewrites": [],
        "context_rewrites": rewrites
    } })
}

/// A path written `a.@.b|[T]`: a key per name, `@` for any index, and a key's type condition after
/// `|`.
fn path(written: &str) -> Value {
    written
        .split('.')
        .map(|element| {
            let (key, condition) = match element.split_once('|') {
                Some((key, condition)) => (
                    key,
                    json!(
                        condition
                            .trim_start_matches('[')
                            .trim_end_matches(']')
                            .split(',')
                            .collect::<Vec<_>>()
                    ),
                ),
                None => (element, Value::Null),
            };
            if key == "@" {
                json!({ "AnyIndex": condition })
            } else {
                json!({ "Key": [key, condition] })
            }
        })
        .collect()
}

fn flatten(at: &str, node: Value) -> Value {
    json!({ "Flatten": { "path": path(at), "node": node } })
}

fn sequence(nodes: Vec<Value>) -> Value {
    json!({ "Sequence": { "nodes": nodes } })
}

/// A context rewrite binding `$binding` to what `elements` reach.
fn rewrite(elements: Vec<Value>, binding: &str) -> Value {
    json!({ "KeyRenamer": { "path": elements, "rename_key_to": binding } })
}

/// A rewrite climbing `climb` levels to an object of `type_name`, reading its `leaf`, bound to
/// `$binding`.
fn reading(climb: usize, type_name: &str, leaf: &str, binding: &str) -> Value {
    let mut elements = vec![json!("Parent"); climb];
    elements.push(json!({ "TypenameEquals": type_name }));
    elements.push(json!({ "Key": [leaf, null] }));
    rewrite(elements, binding)
}

fn check(fixture: &Fixture, plan: Value, operation: &str) -> Result<(), CorrectnessError> {
    fixture.check(&fixture.written_plan(&plan.to_string()), operation)
}

#[track_caller]
fn assert_rejected(result: Result<(), CorrectnessError>, reason: &str) {
    let error = result
        .expect_err("expected the check to reject this plan")
        .to_string();
    assert!(error.contains(reason), "{error}");
}

const UNBOUND: &str = "which no context rewrite binds";
const NO_VARIABLE: &str = "is passed no variable";
const NOT_DECLARED: &str = "which is not a leaf of the declared";
const NOT_FETCHED: &str = "is not always fetched where the fetch runs";
const ROOT_FETCH: &str = "a fetch with no `requires` runs no context rewrite";

//==================================================================================================
// Contextual arguments: bound, and read from their declaration

/// `directory` serves regions holding stores holding departments, and outlets; `stores` resolves
/// an `Offer`, its `label` taking `locale` from the context `locality` that `Store` and
/// `Department` set, and its `price` taking `locale` and `currency`; `outlets` sets `locality` on
/// the interface `Outlet`, its `Offer.tag` taking `locale`.
fn stores() -> Fixture {
    Fixture::new(include_str!("../testdata/context_arguments.graphql"))
}

const OFFER_KEY: &str = "offers { __typename id }";

/// What `directory` fetches: every value a context may be read from, and each offer's key, the
/// store's `locale` fetched as `store_locale`.
fn directory_fetch_with(store_locale: &str) -> Value {
    root_fetch(
        "directory",
        &format!(
            "{{ regions {{ locale stores {{ id {store_locale} currency {OFFER_KEY} \
             departments {{ locale currency {OFFER_KEY} }} }} }} \
             outlets {{ __typename locale currency {OFFER_KEY} }} }}"
        ),
        Vec::new(),
    )
}

fn directory_fetch() -> Value {
    directory_fetch_with("locale")
}

const AT_STORE: &str = "regions.@.stores.@.offers.@";
const AT_DEPARTMENT: &str = "regions.@.stores.@.departments.@.offers.@";
const AT_OUTLET: &str = "outlets.@.offers.@";

/// The client asks for `field` of the offers at `at`: the path's keys, nested.
fn offer_operation(at: &str, field: &str) -> String {
    at.split('.')
        .filter(|element| *element != "@")
        .rev()
        .fold(field.to_string(), |inner, key| {
            format!("{key} {{ {inner} }}")
        })
}

/// The directory, then `subgraph` asked for `field` of the offers at `at`, `field` written with
/// the arguments it passes `variables`, which `rewrites` bind.
fn offer_plan(
    directory: Value,
    at: &str,
    subgraph: &str,
    field: &str,
    variables: &[&str],
    rewrites: Vec<Value>,
) -> Value {
    sequence(vec![
        directory,
        flatten(
            at,
            entity_fetch(
                subgraph,
                &["Offer"],
                &format!("... on Offer {{ {field} }}"),
                variables,
                rewrites,
            ),
        ),
    ])
}

/// The whole checker over the plan asking `stores` for an offer's `label` at `at`, after
/// `directory`, the client asking for `label`.
fn label_checked(
    directory: Value,
    at: &str,
    label: &str,
    variables: &[&str],
    rewrites: Vec<Value>,
) -> Result<(), CorrectnessError> {
    check(
        &stores(),
        offer_plan(directory, at, "stores", label, variables, rewrites),
        &format!("{{ {} }}", offer_operation(at, "label")),
    )
}

const LABEL_LOCAL: &str = "label(locale: $local)";

// `label` is passed `$local`, which a rewrite binds to `locale` read off the store one level up,
// an object setting `locality`.
#[test]
fn contextual_argument_bound_and_read_is_accepted() {
    label_checked(
        directory_fetch(),
        AT_STORE,
        LABEL_LOCAL,
        &["local"],
        vec![reading(1, "Store", "locale", "local")],
    )
    .unwrap();
}

// `label` is selected with no `locale` and the fetch declares no rewrite, so `stores` resolves it
// with no value where its declaration takes the store's. Nothing but the binding obligation sees
// this: the fetch declares no rewrite to be checked.
#[test]
fn argument_and_rewrite_both_omitted_is_rejected() {
    assert_rejected(
        label_checked(directory_fetch(), AT_STORE, "label", &[], Vec::new()),
        NO_VARIABLE,
    );
}

// `label` is passed a variable no rewrite binds, which reaches `stores` with no value.
#[test]
fn argument_passed_with_no_rewrite_is_rejected() {
    assert_rejected(
        label_checked(
            directory_fetch(),
            AT_STORE,
            LABEL_LOCAL,
            &["local"],
            Vec::new(),
        ),
        UNBOUND,
    );
}

// `label` is passed a literal where its declaration takes the context's value: a literal is no
// variable a rewrite binds.
#[test]
fn argument_passed_a_literal_is_rejected() {
    assert_rejected(
        label_checked(
            directory_fetch(),
            AT_STORE,
            r#"label(locale: "en")"#,
            &[],
            Vec::new(),
        ),
        NO_VARIABLE,
    );
}

/// The client asks for the `label` of the stores' offers in the `short` format; `stores` is
/// passed `$local` for `locale` beside `fetched` for `format`.
fn format_checked(fetched: &str) -> Result<(), CorrectnessError> {
    check(
        &stores(),
        offer_plan(
            directory_fetch(),
            AT_STORE,
            "stores",
            &format!(r#"label(locale: $local, format: "{fetched}")"#),
            &["local"],
            vec![reading(1, "Store", "locale", "local")],
        ),
        r#"{ regions { stores { offers { label(format: "short") } } } }"#,
    )
}

// What the fetch contributes is read with the contextual argument dropped and the client's own
// `format` kept.
#[test]
fn ordinary_argument_beside_a_contextual_one_is_accepted() {
    format_checked("short").unwrap();
}

// Dropping the contextual argument leaves `format` to compare: `long` is not what the client asked
// for.
#[test]
fn ordinary_argument_mismatch_is_rejected() {
    assert_rejected(
        format_checked("long"),
        "does not fetch everything the operation requests",
    );
}

// The rewrite climbs to the store, which sets `locality`, but reads its `id` where the declaration
// selects `{ locale }`. The `id` is fetched, so only the declaration tells.
#[test]
fn rewrite_reading_another_leaf_is_rejected() {
    assert_rejected(
        label_checked(
            directory_fetch(),
            AT_STORE,
            LABEL_LOCAL,
            &["local"],
            vec![reading(1, "Store", "id", "local")],
        ),
        NOT_DECLARED,
    );
}

// `directory` put the store's `currency` under the response key `locale`, and the rewrite reads
// that key: the router forwards the currency, whatever the key is called, so it is the field under
// the key that is compared with the declaration.
#[test]
fn aliased_wrong_field_is_rejected() {
    assert_rejected(
        label_checked(
            directory_fetch_with("locale: currency"),
            AT_STORE,
            LABEL_LOCAL,
            &["local"],
            vec![reading(1, "Store", "locale", "local")],
        ),
        NOT_DECLARED,
    );
}

// `directory` put the store's `locale` under the response key `ctxLocale`, and the rewrite reads
// that key, under which the declared field sits.
#[test]
fn aliased_declared_field_is_accepted() {
    label_checked(
        directory_fetch_with("ctxLocale: locale"),
        AT_STORE,
        LABEL_LOCAL,
        &["local"],
        vec![reading(1, "Store", "ctxLocale", "local")],
    )
    .unwrap();
}

// A type guard after the last key, on the value itself: the `locale` read is a `String`, no object
// matches the guard, and the router supplies no argument.
#[test]
fn type_guard_after_the_leaf_is_rejected() {
    assert_rejected(
        label_checked(
            directory_fetch(),
            AT_STORE,
            LABEL_LOCAL,
            &["local"],
            vec![rewrite(
                vec![
                    json!("Parent"),
                    json!({ "TypenameEquals": "Store" }),
                    json!({ "Key": ["locale", null] }),
                    json!({ "TypenameEquals": "Offer" }),
                ],
                "local",
            )],
        ),
        NOT_FETCHED,
    );
}

// `directory` fetches each outlet's values on its member: the stores' offers, and under the
// departments their `currency` keyed as `locale`. No department's offers are fetched, so the fetch
// runs at the stores' offers alone, and its rewrite, written for either member, reads the store's
// `locale`. The department's `locale` key, which holds its currency, is one no execution of the
// fetch reads, and is not judged.
#[test]
fn rewrite_reads_only_where_the_fetch_runs_is_accepted() {
    let directory = root_fetch(
        "directory",
        &format!(
            "{{ outlets {{ __typename ... on Store {{ locale {OFFER_KEY} }} \
             ... on Department {{ locale: currency }} }} }}"
        ),
        Vec::new(),
    );
    check(
        &stores(),
        offer_plan(
            directory,
            AT_OUTLET,
            "stores",
            LABEL_LOCAL,
            &["local"],
            vec![rewrite(
                vec![json!("Parent"), json!({ "Key": ["locale", null] })],
                "local",
            )],
        ),
        "{ outlets { ... on Store { offers { label } } } }",
    )
    .unwrap();
}

const PRICE: &str = "price(locale: $local, currency: $money)";

// Each of `price`'s arguments is bound by its own rewrite to the leaf its declaration selects off
// the store.
#[test]
fn two_arguments_each_bound_to_their_own_leaf_are_accepted() {
    check(
        &stores(),
        offer_plan(
            directory_fetch(),
            AT_STORE,
            "stores",
            PRICE,
            &["local", "money"],
            vec![
                reading(1, "Store", "locale", "local"),
                reading(1, "Store", "currency", "money"),
            ],
        ),
        &format!("{{ {} }}", offer_operation(AT_STORE, "price")),
    )
    .unwrap();
}

// Each variable is bound to the other's leaf, so `stores` is sent the currency as `locale` and
// the locale as `currency`. Both are `String` and both are fetched, so only the declaration tells.
#[test]
fn two_arguments_bound_to_each_others_leaves_are_rejected() {
    assert_rejected(
        check(
            &stores(),
            offer_plan(
                directory_fetch(),
                AT_STORE,
                "stores",
                PRICE,
                &["local", "money"],
                vec![
                    reading(1, "Store", "currency", "local"),
                    reading(1, "Store", "locale", "money"),
                ],
            ),
            &format!("{{ {} }}", offer_operation(AT_STORE, "price")),
        ),
        NOT_DECLARED,
    );
}

// The rewrite climbs past the store to the region, which does not set `locality`, and reads the
// region's own `locale`, which is fetched.
#[test]
fn rewrite_climbing_to_a_non_setter_is_rejected() {
    assert_rejected(
        label_checked(
            directory_fetch(),
            AT_STORE,
            LABEL_LOCAL,
            &["local"],
            vec![reading(2, "Region", "locale", "local")],
        ),
        "climbs to a `Region`, which sets no context `locality`",
    );
}

// The rewrite reads `locale` off the department, the nearest of the two objects above the offer
// that set `locality`.
#[test]
fn rewrite_reading_the_nearest_setter_is_accepted() {
    label_checked(
        directory_fetch(),
        AT_DEPARTMENT,
        LABEL_LOCAL,
        &["local"],
        vec![reading(1, "Department", "locale", "local")],
    )
    .unwrap();
}

// An outlet is a store or a department, both setting `locality`: one variable is bound by a
// rewrite per type, each reading the declared leaf, and the router takes the value from the one
// that finds it.
#[test]
fn one_rewrite_per_setter_is_accepted() {
    label_checked(
        directory_fetch(),
        AT_OUTLET,
        LABEL_LOCAL,
        &["local"],
        vec![
            reading(1, "Store", "locale", "local"),
            reading(1, "Department", "locale", "local"),
        ],
    )
    .unwrap();
}

// Only the store's rewrite remains, so an offer under a department is sent no `locale`.
#[test]
fn one_setter_left_unread_is_rejected() {
    assert_rejected(
        label_checked(
            directory_fetch(),
            AT_OUTLET,
            LABEL_LOCAL,
            &["local"],
            vec![reading(1, "Store", "locale", "local")],
        ),
        NOT_FETCHED,
    );
}

// The department's rewrite reads `currency` where the declaration selects `{ locale }`. Every
// outlet still binds a value, so only the declaration tells.
#[test]
fn one_setter_misread_is_rejected() {
    assert_rejected(
        label_checked(
            directory_fetch(),
            AT_OUTLET,
            LABEL_LOCAL,
            &["local"],
            vec![
                reading(1, "Store", "locale", "local"),
                reading(1, "Department", "currency", "local"),
            ],
        ),
        NOT_DECLARED,
    );
}

// `outlets` declares `locality` on the interface `Outlet`, which is set by its implementations:
// the rewrites read off them, and the declaration's `locale` is read on the interface.
#[test]
fn context_declared_on_an_interface_is_read_on_its_implementations_is_accepted() {
    check(
        &stores(),
        offer_plan(
            directory_fetch(),
            AT_OUTLET,
            "outlets",
            "tag(locale: $local)",
            &["local"],
            vec![
                reading(1, "Store", "locale", "local"),
                reading(1, "Department", "locale", "local"),
            ],
        ),
        &format!("{{ {} }}", offer_operation(AT_OUTLET, "tag")),
    )
    .unwrap();
}

/// `Outlet` as a union of `Store` and `Department`: `stores` sets `locality` on both members, its
/// `Offer.label` taking `locale`; `badges` sets it on the union, its `Offer.badge` taking the
/// outlet's `__typename`.
fn union_stores() -> Fixture {
    Fixture::new(include_str!("../testdata/context_arguments_union.graphql"))
}

/// The whole checker over the union, asking `subgraph` for `field` below the outlets after
/// `directory` fetched `selected` and the offers' keys on each member.
fn union_checked(
    selected: &str,
    subgraph: &str,
    field: &str,
    client_field: &str,
    rewrites: Vec<Value>,
) -> Result<(), CorrectnessError> {
    let directory = root_fetch(
        "directory",
        &format!(
            "{{ outlets {{ ... on Store {{ {selected} {OFFER_KEY} }} \
             ... on Department {{ {selected} {OFFER_KEY} }} }} }}"
        ),
        Vec::new(),
    );
    check(
        &union_stores(),
        offer_plan(directory, AT_OUTLET, subgraph, field, &["local"], rewrites),
        &format!(
            "{{ outlets {{ ... on Store {{ offers {{ {client_field} }} }} \
             ... on Department {{ offers {{ {client_field} }} }} }} }}"
        ),
    )
}

fn union_label_checked(rewrites: Vec<Value>) -> Result<(), CorrectnessError> {
    union_checked("locale currency", "stores", LABEL_LOCAL, "label", rewrites)
}

// Both members of the union set `locality`, and a rewrite per member reads the declared leaf off
// it.
#[test]
fn one_rewrite_per_union_member_is_accepted() {
    union_label_checked(vec![
        reading(1, "Store", "locale", "local"),
        reading(1, "Department", "locale", "local"),
    ])
    .unwrap();
}

// Only the store's rewrite remains, so an offer under a department of the union is sent no
// `locale`.
#[test]
fn union_member_left_unread_is_rejected() {
    assert_rejected(
        union_label_checked(vec![reading(1, "Store", "locale", "local")]),
        NOT_FETCHED,
    );
}

// The department's rewrite reads `currency`, and the value is there: only the declaration tells,
// and only a path follower that keeps the member's object below the union's fragment reaches it.
#[test]
fn union_member_misread_is_rejected() {
    assert_rejected(
        union_label_checked(vec![
            reading(1, "Store", "locale", "local"),
            reading(1, "Department", "currency", "local"),
        ]),
        NOT_DECLARED,
    );
}

// A context declared on a union selects its `__typename`, which every union has though it declares
// no field, and each member's fetched `__typename` is read by the rewrite on it.
#[test]
fn context_on_a_union_reads_its_typename_is_accepted() {
    union_checked(
        "__typename",
        "badges",
        "badge(kind: $local)",
        "badge",
        vec![
            reading(1, "Store", "__typename", "local"),
            reading(1, "Department", "__typename", "local"),
        ],
    )
    .unwrap();
}

/// The whole checker over a plan of one root fetch asking `stores` for the `label` of every
/// store's offers, written `label`, with `rewrites` beside it.
fn root_label_checked(label: &str, rewrites: Vec<Value>) -> Result<(), CorrectnessError> {
    let declarations = if label.contains('$') {
        "query($local: String) "
    } else {
        ""
    };
    check(
        &stores(),
        root_fetch(
            "stores",
            &format!("{declarations}{{ regions {{ stores {{ offers {{ {label} }} }} }} }}"),
            rewrites,
        ),
        &format!("{{ {} }}", offer_operation(AT_STORE, "label")),
    )
}

// A root fetch passes `label` the variable a rewrite binds to the store's `locale`, but the router
// runs a fetch's context rewrites only when it sends representations, so `stores` gets no value.
#[test]
fn argument_passed_by_a_root_fetch_is_rejected() {
    assert_rejected(
        root_label_checked(LABEL_LOCAL, vec![reading(1, "Store", "locale", "local")]),
        ROOT_FETCH,
    );
}

// A root fetch selects `label` with neither argument nor rewrite, which only the root obligation
// sees.
#[test]
fn argument_omitted_by_a_root_fetch_is_rejected() {
    assert_rejected(root_label_checked("label", Vec::new()), ROOT_FETCH);
}

//==================================================================================================
// A contextual value read off the object the fetch runs on

/// `Query.item` is an `Item`, a `Product` or an `Other`, served by `catalog`; `details` resolves
/// both and serves their `details`, whose `label` takes `title` from the context `product` both
/// set, and whose `summary` takes `info { note }`. The planner selects `label` in the fetch at the
/// item, and the rewrite reads `title` off the object that fetch runs on, which the router sends
/// only for the types its `requires` entries are written at.
fn own_position() -> Fixture {
    Fixture::new(include_str!("../testdata/context_own_position.graphql"))
}

/// The key of a member, with `fetched` beside it.
fn member(type_name: &str, fetched: &str) -> String {
    format!("... on {type_name} {{ __typename id {fetched} }}")
}

/// `catalog` asked for `holder` holding `members`, then `details` asked for `field` of the details
/// of the entities at `at`, its `requires` written at `entry_types`, `field` passing `$ctx`, which
/// `rewrite` binds; the client asks for `client_field` of the details of each entry type.
fn own_position_checked(
    holder: &str,
    at: &str,
    members: &[String],
    entry_types: &[&str],
    field: &str,
    rewrite: Value,
    client_field: &str,
) -> Result<(), CorrectnessError> {
    let cases: String = entry_types
        .iter()
        .map(|type_name| format!("... on {type_name} {{ details {{ {field} }} }} "))
        .collect();
    let operation: String = entry_types
        .iter()
        .map(|type_name| format!("... on {type_name} {{ details {{ {client_field} }} }} "))
        .collect();
    check(
        &own_position(),
        sequence(vec![
            root_fetch(
                "catalog",
                &format!("{{ {holder} {{ __typename {} }} }}", members.join(" ")),
                Vec::new(),
            ),
            flatten(
                at,
                entity_fetch("details", entry_types, &cases, &["ctx"], vec![rewrite]),
            ),
        ]),
        &format!("{{ {holder} {{ {operation}}} }}"),
    )
}

/// [`own_position_checked`] reading `title` off the object the fetch runs on, for the `label`.
fn label_off_own_object(
    holder: &str,
    at: &str,
    members: &[String],
    entry_types: &[&str],
) -> Result<(), CorrectnessError> {
    own_position_checked(
        holder,
        at,
        members,
        entry_types,
        "label(title: $ctx)",
        rewrite(vec![json!({ "Key": ["title", null] })], "ctx"),
        "label",
    )
}

// The fetch is sent the products alone, each of which `catalog` answered with `title` under
// `... on Product`, and reads it off the object it runs on.
#[test]
fn value_off_the_object_the_fetch_runs_on_is_accepted() {
    label_off_own_object("item", "item", &[member("Product", "title")], &["Product"]).unwrap();
}

// The same over a list of items: the level the fetch runs on holds an `Other` too, which is sent no
// representation and owes no value.
#[test]
fn value_off_the_objects_the_fetch_runs_on_in_a_list_is_accepted() {
    label_off_own_object(
        "items",
        "items.@",
        &[member("Product", "title")],
        &["Product"],
    )
    .unwrap();
}

// `catalog` fetched no `title` for the products, so the value is missing where the fetch runs.
#[test]
fn value_unfetched_for_the_object_the_fetch_runs_on_is_rejected() {
    assert_rejected(
        label_off_own_object("item", "item", &[member("Product", "")], &["Product"]),
        NOT_FETCHED,
    );
}

// The `requires` written at both members, with `title` fetched under each: every object the fetch
// is sent has its value.
#[test]
fn value_fetched_under_each_member_sent_is_accepted() {
    label_off_own_object(
        "item",
        "item",
        &[member("Product", "title"), member("Other", "title")],
        &["Product", "Other"],
    )
    .unwrap();
}

// The same over a list of items.
#[test]
fn value_fetched_under_each_member_sent_in_a_list_is_accepted() {
    label_off_own_object(
        "items",
        "items.@",
        &[member("Product", "title"), member("Other", "title")],
        &["Product", "Other"],
    )
    .unwrap();
}

// The `requires` written at both members, with `title` fetched under the product alone: an `Other`
// the fetch is sent has no value.
#[test]
fn value_missing_under_one_member_sent_is_rejected() {
    assert_rejected(
        label_off_own_object(
            "item",
            "item",
            &[member("Product", "title"), member("Other", "")],
            &["Product", "Other"],
        ),
        NOT_FETCHED,
    );
}

// `catalog` fetched `otherTitle` under the key `title` for the products: the value is there where
// the fetch runs, and it is the wrong field, which only the reading tells.
#[test]
fn wrong_field_off_the_object_the_fetch_runs_on_is_rejected() {
    assert_rejected(
        label_off_own_object(
            "item",
            "item",
            &[member("Product", "title: otherTitle")],
            &["Product"],
        ),
        NOT_DECLARED,
    );
}

/// The plan reading `info { note }` for the product's `summary` at `concrete`, with the product's
/// `info` fetched as `info`.
fn summary_checked(info: &str) -> Result<(), CorrectnessError> {
    own_position_checked(
        "concrete",
        "concrete",
        &[member("Product", info)],
        &["Product"],
        "summary(note: $ctx)",
        rewrite(
            vec![
                json!({ "Key": ["info", null] }),
                json!({ "Key": ["note", null] }),
            ],
            "ctx",
        ),
        "summary",
    )
}

// `note` fetched under `... on Detail`: narrowed at a level below the levels the two paths share,
// which the fetch's path says nothing about, it serves nothing, since the product's `info` may be a
// `Summary`.
#[test]
fn value_narrowed_below_the_shared_levels_is_rejected() {
    assert_rejected(
        summary_checked("info { __typename ... on Detail { note } }"),
        NOT_FETCHED,
    );
}

// `note` fetched on the interface serves every `info`.
#[test]
fn value_unnarrowed_below_the_shared_levels_is_accepted() {
    summary_checked("info { note }").unwrap();
}
