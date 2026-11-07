//! Reading contextual values through union members and path conditions.
//!
//! Each plan is written inline in the shape the planner gives its operation, with the part a case
//! varies edited in: which rewrites bind the contextual argument, where the fetches select what
//! the value is read from, and the type condition on a path. Every fetch reading a contextual
//! value binds it to `$contextual`, one rewrite per runtime type the context object may have, and
//! the router takes the value from whichever rewrite resolves.

use super::CorrectnessError;
use super::Fixture;

/// The plan for a sequence of nodes.
fn sequence(nodes: &[String]) -> String {
    format!(r#"{{ "Sequence": {{ "nodes": [{}] }} }}"#, nodes.join(", "))
}

/// The root fetch to `subgraph`.
fn root_fetch(subgraph: &str, operation: &str) -> String {
    format!(
        r#"{{ "Fetch": {{
            "subgraph_name": "{subgraph}",
            "variable_usages": [],
            "operation_document": "{operation}",
            "operation_kind": "query",
            "input_rewrites": [],
            "output_rewrites": [],
            "context_rewrites": []
        }} }}"#
    )
}

/// A context rewrite binding `$contextual` to what `path` reaches.
fn contextual(path: &str) -> String {
    format!(r#"{{ "KeyRenamer": {{ "path": [{path}], "rename_key_to": "contextual" }} }}"#)
}

/// An entity fetch to `subgraph` at `path`, resolving `entity` by `id` and selecting `selection`
/// on it; it is passed `$contextual` when `context_rewrites` binds it.
fn entity_fetch(
    path: &str,
    subgraph: &str,
    entity: &str,
    selection: &str,
    input_rewrites: &str,
    context_rewrites: &[String],
) -> String {
    let (variables, usages) = if context_rewrites.is_empty() {
        ("", "")
    } else {
        (", $contextual: String", r#""contextual""#)
    };
    format!(
        r#"{{ "Flatten": {{
            "path": [{path}],
            "node": {{ "Fetch": {{
                "subgraph_name": "{subgraph}",
                "variable_usages": [{usages}],
                "requires": [{{
                    "kind": "InlineFragment",
                    "typeCondition": "{entity}",
                    "selections": [
                        {{ "kind": "Field", "name": "__typename" }},
                        {{ "kind": "Field", "name": "id" }}
                    ]
                }}],
                "operation_document": "query($representations: [_Any!]!{variables}) {{ _entities(representations: $representations) {{ ... on {entity} {{ {selection} }} }} }}",
                "operation_kind": "query",
                "input_rewrites": {input_rewrites},
                "output_rewrites": [],
                "context_rewrites": [{}]
            }} }}
        }} }}"#,
        context_rewrites.join(", ")
    )
}

/// Asserts the plan was rejected for a contextual value missing for data of `runtime_type`.
fn assert_context_value_missing(error: impl std::fmt::Display, runtime_type: &str) {
    let error = error.to_string();
    assert!(
        error.contains("the contextual value for `$contextual` is not always fetched")
            && error.contains(&format!("for data of: Query / {runtime_type}")),
        "{error}"
    );
}

//==================================================================================================
// A contextual value below a union's members

fn outlets_fixture() -> Fixture {
    Fixture::new(include_str!("../testdata/context_outlets.graphql"))
}

/// The root fetch selects, on either member, both values the context is read from and the keys of
/// its featured product and of its products; then `field` is fetched for the products at `path`,
/// bound by `rewrites`.
fn outlet_product_field_plan(path: &str, field: &str, rewrites: &[String]) -> String {
    let member = |member: &str| {
        format!(
            "... on {member} {{ __typename locale address {{ locale }} \
             featured {{ __typename id }} products {{ __typename id }} }}"
        )
    };
    sequence(&[
        root_fetch(
            "outlets",
            &format!(
                "{{ outlets {{ __typename {} {} }} }}",
                member("Store"),
                member("Kiosk")
            ),
        ),
        entity_fetch(
            path,
            "prices",
            "Product",
            &format!("{field}(locale: $contextual)"),
            "[]",
            rewrites,
        ),
    ])
}

/// Every product's `price`: the fetch runs below `products`, which only the members declare.
fn price_plan(rewrites: &[String]) -> String {
    outlet_product_field_plan(
        r#"{ "Key": ["outlets", null] }, { "AnyIndex": null }, { "Key": ["products", null] }, { "AnyIndex": null }"#,
        "price",
        rewrites,
    )
}

/// The featured product's `delivery`: the fetch runs at `featured`, which ends its path, and the
/// rewrites descend past `address`.
fn delivery_plan(rewrites: &[String]) -> String {
    outlet_product_field_plan(
        r#"{ "Key": ["outlets", null] }, { "AnyIndex": null }, { "Key": ["featured", null] }"#,
        "delivery",
        rewrites,
    )
}

/// Climbs from a product to its outlet and reads `locale` off one member.
fn locale_on(member: &str) -> String {
    contextual(&format!(
        r#""Parent", {{ "TypenameEquals": "{member}" }}, {{ "Key": ["locale", null] }}"#
    ))
}

/// Climbs from a product to its outlet and reads its address's `locale` off one member.
fn address_locale_on(member: &str) -> String {
    contextual(&format!(
        r#""Parent", {{ "TypenameEquals": "{member}" }}, {{ "Key": ["address", null] }}, {{ "Key": ["locale", null] }}"#
    ))
}

fn check_outlets(plan: &str, holder: &str, field: &str) -> Result<(), CorrectnessError> {
    let fixture = outlets_fixture();
    let held = format!("{holder} {{ {field} }}");
    fixture.check(
        &fixture.written_plan(plan),
        &format!("{{ outlets {{ ... on Store {{ {held} }} ... on Kiosk {{ {held} }} }} }}"),
    )
}

// The router climbs from each product to its outlet and takes `locale` from whichever rewrite's
// `__typename` matches, so every product is sent its own outlet's `locale`. The fetch runs below
// `products`, which only the members declare: its positions are found inside the root fetch's
// fragments on `Store` and `Kiosk`, each covered by that member's rewrite.
#[test]
fn context_rewrite_per_union_member_is_accepted() {
    check_outlets(
        &price_plan(&[locale_on("Store"), locale_on("Kiosk")]),
        "products",
        "price",
    )
    .unwrap();
}

// With no rewrite for `Kiosk`, a kiosk's products are sent no `locale`: the store's rewrite finds
// nothing on a kiosk. The fetch's positions under the fragment on `Kiosk` have to be found, by
// looking `products` up on `Kiosk` rather than on the union, for that to be seen.
#[test]
fn union_member_without_a_context_rewrite_is_rejected() {
    let error = check_outlets(&price_plan(&[locale_on("Store")]), "products", "price")
        .expect_err("a kiosk's products are sent no `locale`");
    assert_context_value_missing(error, "Kiosk");
}

// Every featured product is sent its outlet's address's `locale`. The fetch's position ends at
// `featured` and needs no lookup, but each rewrite descends past `address`, a field only the member
// it reads off declares, so this is the value's side of the same lookup.
#[test]
fn context_value_below_a_union_members_field_is_accepted() {
    check_outlets(
        &delivery_plan(&[address_locale_on("Store"), address_locale_on("Kiosk")]),
        "featured",
        "delivery",
    )
    .unwrap();
}

// With only the store's rewrite, a kiosk's featured product is sent no `locale`: the rewrite reads
// `address` off a `Store` alone, and that narrowing is carried past `address` to the value.
#[test]
fn context_value_below_another_union_members_field_is_rejected() {
    let error = check_outlets(
        &delivery_plan(&[address_locale_on("Store")]),
        "featured",
        "delivery",
    )
    .expect_err("a kiosk's featured product is sent no `locale`");
    assert_context_value_missing(error, "Kiosk");
}

//==================================================================================================
// A contextual value under a fragment that states its path condition

fn owner_profile_fixture() -> Fixture {
    Fixture::new(include_str!("../testdata/context_owner_profile.graphql"))
}

/// The root fetch selects each owner's profile key inside a fragment on `Owner`, the profiles'
/// `locale` is fetched at `stores.@.owner|owners.profile`, and then every product's `price` reads
/// it through its store.
fn owner_price_plan(owners: &str) -> String {
    sequence(&[
        owner_stores_fetch(),
        locale_fetch(owners),
        owner_price_fetch(),
    ])
}

/// [`owner_price_plan`] with the root fetch selecting `profiles` under each owner.
fn owner_price_plan_selecting(profiles: &str, owners: &str) -> String {
    sequence(&[
        stores_fetch(profiles),
        locale_fetch(owners),
        owner_price_fetch(),
    ])
}

/// The root fetch, selecting `profiles` under each store's owner.
fn stores_fetch(profiles: &str) -> String {
    root_fetch(
        "stores",
        &format!(
            "{{ stores {{ __typename owner {{ __typename {profiles} }} \
             products {{ __typename id }} }} }}"
        ),
    )
}

/// The profile's `locale`, fetched for the owners of the types `owners` names.
fn locale_fetch(owners: &str) -> String {
    entity_fetch(
        &format!(
            r#"{{ "Key": ["stores", null] }}, {{ "AnyIndex": null }}, {{ "Key": ["owner", [{owners}]] }}, {{ "Key": ["profile", null] }}"#
        ),
        "profiles",
        "Profile",
        "locale",
        "[]",
        &[],
    )
}

/// Every product's `price`, its `locale` read off its store's owner's profile through the store.
fn owner_price_fetch() -> String {
    entity_fetch(
        r#"{ "Key": ["stores", null] }, { "AnyIndex": null }, { "Key": ["products", null] }, { "AnyIndex": null }"#,
        "stores",
        "Product",
        "price(locale: $contextual)",
        "[]",
        &[contextual(
            r#""Parent", { "TypenameEquals": "Store" }, { "Key": ["owner", null] }, { "Key": ["profile", null] }, { "Key": ["locale", null] }"#,
        )],
    )
}

fn check_every_store_price(fixture: &Fixture, plan: &str) -> Result<(), CorrectnessError> {
    fixture.check(
        &fixture.written_plan(plan),
        "{ stores { products { price } } }",
    )
}

// `owner|[Person, Company]` admits every owner, so the router fetches `locale` for every
// profile, and each product reads it off its own store whatever the owner is. The root fetch's
// fragment on `Owner` reaches exactly those two types, so the value sits under that fragment once,
// rather than once per type the path names; guarding it by both names would mount a copy narrowed
// to each instead, siblings that serve the same (`split_owner_context_is_accepted`).
#[test]
fn context_value_under_a_fragment_stating_its_condition_is_accepted() {
    let fixture = owner_profile_fixture();
    check_every_store_price(&fixture, &owner_price_plan(r#""Person", "Company""#)).unwrap();
}

// `owner|[Person]` fetches `locale` only for a person's profile, so a company's products are sent
// none. The fragment on `Owner` also reaches `Company`, which the path excludes, so the value it
// holds stays narrowed to `Person`.
#[test]
fn context_value_missing_for_one_owner_type_is_rejected() {
    let fixture = owner_profile_fixture();
    let error = check_every_store_price(&fixture, &owner_price_plan(r#""Person""#))
        .expect_err("a company's products are sent no `locale`");
    assert_context_value_missing(error, "Store");
}

/// Each owner's profile key selected separately under `Person` and under `Company`.
const SPLIT_PROFILES: &str = "... on Person { profile { __typename id } } \
                              ... on Company { profile { __typename id } }";

// The `locale` fetched for both owner types under the separate selections: each copy is narrowed
// to its owner type past the levels the two paths share, but the two are siblings, alike but at
// the owner, and together serve every product.
#[test]
fn split_owner_context_is_accepted() {
    let fixture = owner_profile_fixture();
    check_every_store_price(
        &fixture,
        &owner_price_plan_selecting(SPLIT_PROFILES, r#""Person", "Company""#),
    )
    .unwrap();
}

// The `locale` fetched under the separate selections for a `Person` alone: a company's products
// are sent none, and the `Person` copy has no sibling to cover them.
#[test]
fn missing_owner_context_is_rejected() {
    let fixture = owner_profile_fixture();
    let error = check_every_store_price(
        &fixture,
        &owner_price_plan_selecting(SPLIT_PROFILES, r#""Person""#),
    )
    .expect_err("a company's products are sent no `locale`");
    assert_context_value_missing(error, "Store");
}

/// The root fetch, the `locale` for persons, then `company`, then every price: one `locale` fetch
/// per owner type.
fn locale_per_owner_plan(company: &str) -> String {
    sequence(&[
        owner_stores_fetch(),
        locale_fetch(r#""Person""#),
        company.to_string(),
        owner_price_fetch(),
    ])
}

/// The root fetch selecting each owner's profile key inside a fragment on `Owner`.
fn owner_stores_fetch() -> String {
    stores_fetch("... on Owner { profile { __typename id } }")
}

// Each fetch mounts the `locale` under its own path condition, a copy narrowed to its owner type;
// the two are siblings and together serve every product.
#[test]
fn locale_fetched_per_owner_type_is_accepted() {
    let fixture = owner_profile_fixture();
    check_every_store_price(
        &fixture,
        &locale_per_owner_plan(&locale_fetch(r#""Company""#)),
    )
    .unwrap();
}

// The companies' `locale` fetched only where `$company` is true: the two copies are guarded
// differently, so they are no siblings, and where `$company` is false a company's products are
// sent none.
#[test]
fn locale_fetched_per_owner_type_under_different_guards_is_rejected() {
    let fixture = owner_profile_fixture();
    let company = format!(
        r#"{{ "Condition": {{
            "condition_variable": "company",
            "if_clause": {},
            "else_clause": null
        }} }}"#,
        locale_fetch(r#""Company""#)
    );
    let error = check_every_store_price(&fixture, &locale_per_owner_plan(&company))
        .expect_err("where `$company` is false a company's products are sent no `locale`");
    assert_context_value_missing(error, "Store");
}

//==================================================================================================
// A path condition's names, compared as the router compares them

fn owner_label_fixture() -> Fixture {
    Fixture::new(include_str!("../testdata/context_owner_label.graphql"))
}

/// Every product's `price`, its `label` read off its store's owner through `owner|owners`: the
/// condition is on the value's own path past the store, and the fetch's own path carries none.
fn price_through(owners: Option<&str>) -> String {
    let owners = owners.map_or("null".to_string(), |owners| format!("[{owners}]"));
    entity_fetch(
        r#"{ "Key": ["stores", null] }, { "AnyIndex": null }, { "Key": ["products", null] }, { "AnyIndex": null }"#,
        "catalog",
        "Product",
        "price(label: $contextual)",
        "[]",
        &[contextual(&format!(
            r#""Parent", {{ "TypenameEquals": "Store" }}, {{ "Key": ["owner", {owners}] }}, {{ "Key": ["label", null] }}"#
        ))],
    )
}

/// The root fetch selecting each owner's `label` for every owner, then every product's `price`.
fn read_through_plan(owners: &str) -> String {
    sequence(&[
        root_fetch(
            "catalog",
            "{ stores { __typename owner { __typename label } products { __typename id } } }",
        ),
        price_through(Some(owners)),
    ])
}

/// The root fetch selecting each owner's key, the owners' `label` fetched by `owners` at
/// `stores.@.owner|owners`, then every product's `price`.
fn label_at_plan(owners: &str) -> String {
    sequence(&[
        root_fetch(
            "catalog",
            "{ stores { __typename owner { __typename id } products { __typename id } } }",
        ),
        entity_fetch(
            &format!(
                r#"{{ "Key": ["stores", null] }}, {{ "AnyIndex": null }}, {{ "Key": ["owner", [{owners}]] }}"#
            ),
            "owners",
            "Owner",
            "label",
            r#"[{ "ValueSetter": {
                "path": [{ "TypenameEquals": "Owner" }, { "Key": ["__typename", null] }],
                "set_value_to": "Owner"
            } }]"#,
            &[],
        ),
        price_through(None),
    ])
}

// The router's `iterate_path` compares each name of `owner|[Owner]` with the owner's
// `__typename`, which is `Person` or `Company` and never `Owner`, so the rewrite binds no value and
// every price is asked with none. The `label` itself is fetched for every owner.
#[test]
fn context_value_read_through_an_interface_name_is_rejected() {
    let fixture = owner_label_fixture();
    let error = check_every_store_price(&fixture, &read_through_plan(r#""Owner""#))
        .expect_err("no owner's `__typename` is `Owner`");
    assert_context_value_missing(error, "Store");
}

// The same read through `owner|[Person, Company]`, which every owner's `__typename` matches.
#[test]
fn context_value_read_through_member_names_is_accepted() {
    let fixture = owner_label_fixture();
    check_every_store_price(&fixture, &read_through_plan(r#""Person", "Company""#)).unwrap();
}

// `owners` is sent every owner at `owner|[Person, Company]` and answers `label` inside its
// fragment on `Owner`, which reaches exactly the two names, so every owner has a `label` and every
// product's `price` reads one.
#[test]
fn value_mounted_at_a_condition_its_fragment_states_is_read() {
    let fixture = owner_label_fixture();
    check_every_store_price(&fixture, &label_at_plan(r#""Person", "Company""#)).unwrap();
}

// At `owner|[Person]` the router sends `owners` only the persons, so a company has no `label`
// and its products are read none, though the fetch's own fragment on `Owner` would admit it.
#[test]
fn value_mounted_at_a_condition_missing_a_member_is_rejected() {
    let fixture = owner_label_fixture();
    let error = check_every_store_price(&fixture, &label_at_plan(r#""Person""#))
        .expect_err("a company's products are read no `label`");
    assert_context_value_missing(error, "Store");
}
