//! What a fetch's subgraph resolves, and what its fragments are credited with.
//!
//! Ports of the model's `Resolution` and `FragmentMembership` tests. Most check a whole plan whose
//! client operation is what the plan fetches, so the verdict is the resolvability check's or the
//! narrowing's alone; the rest call the two directly, where the model calls them directly too.

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::executable::Selection;
use apollo_compiler::name;

use super::super::super::CorrectnessError;
use super::super::context::context_uses;
use super::super::resolvable::check_fetch_selects_resolvable;
use super::super::selections::narrow_type_conditions;
use super::super::subgraph::Subgraph;
use super::Fixture;
use crate::query_plan::QueryPlan;
use crate::query_plan::TopLevelPlanNode;
use crate::schema::ValidFederationSchema;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;

/// `shop`, which marks `Vendor.name`, `Vendor.code` and `Gizmo.label` external and opens windows
/// under `topProducts`, `featured` and `Product.vendor`; `catalog`, the same schema with nothing
/// external; and `ledger`, which marks the key `Vendor.id` external.
fn windows() -> Fixture {
    Fixture::new(include_str!("../testdata/provides_windows.graphql"))
}

/// `source` and `other`, which agree on everything but whether `B` implements `I`.
fn membership() -> Fixture {
    Fixture::new(include_str!("../testdata/fragment_membership.graphql"))
}

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

/// A plan of one root fetch, checked against the client operation it fetches.
fn check_root_fetch(
    fixture: &Fixture,
    subgraph: &str,
    operation: &str,
) -> Result<(), CorrectnessError> {
    fixture.check(
        &fixture.written_plan(&root_fetch(subgraph, operation)),
        operation,
    )
}

/// `catalog` fetches each entity's key at `path`, then `subgraph` is asked, under `_entities`,
/// for what `selections` selects of each `entity_type`. An `@` in `path` is a list's index.
fn entity_plan(
    fixture: &Fixture,
    path: &[&str],
    entity_type: &str,
    subgraph: &str,
    selections: &str,
) -> QueryPlan {
    let keys = path
        .iter()
        .rev()
        .filter(|name| **name != "@")
        .fold("__typename id".to_string(), |inner, name| {
            format!("{name} {{ {inner} }}")
        });
    let flatten_path = path
        .iter()
        .map(|name| match *name {
            "@" => r#"{ "AnyIndex": null }"#.to_string(),
            name => format!(r#"{{ "Key": ["{name}", null] }}"#),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let catalog_fetch = root_fetch("catalog", &format!("{{ {keys} }}"));
    fixture.written_plan(&format!(
        r#"{{ "Sequence": {{ "nodes": [
            {catalog_fetch},
            {{ "Flatten": {{
                "path": [{flatten_path}],
                "node": {{ "Fetch": {{
                    "subgraph_name": "{subgraph}",
                    "variable_usages": [],
                    "requires": [{{
                        "kind": "InlineFragment",
                        "typeCondition": "{entity_type}",
                        "selections": [
                            {{ "kind": "Field", "name": "__typename" }},
                            {{ "kind": "Field", "name": "id" }}
                        ]
                    }}],
                    "operation_document": "query($representations: [_Any!]!) {{ _entities(representations: $representations) {{ ... on {entity_type} {{ {selections} }} }} }}",
                    "operation_kind": "query",
                    "input_rewrites": [],
                    "output_rewrites": [],
                    "context_rewrites": []
                }} }}
            }} }}
        ] }} }}"#
    ))
}

/// The selections of `operation` read in `schema`, with the root type they are read from.
fn selections(schema: &ValidFederationSchema, operation: &str) -> (Name, Vec<Selection>) {
    let document = ExecutableDocument::parse_and_validate(
        schema.schema(),
        operation.to_string(),
        "operation.graphql",
    )
    .expect("valid operation");
    let operation = document.operations.get(None).expect("one operation");
    (
        operation.selection_set.ty.clone(),
        operation.selection_set.selections.clone(),
    )
}

/// Whether a fetch to `subgraph` selecting `operation` selects only what the subgraph resolves.
fn check_selects_resolvable(
    fixture: &Fixture,
    subgraph: &str,
    operation: &str,
) -> Result<(), String> {
    let schema = &fixture.subgraphs[subgraph];
    let (root_type, selections) = selections(schema, operation);
    check_fetch_selects_resolvable(subgraph, schema, &root_type, &selections)
        .map_err(|error| error.to_string())
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
// `@provides` windows

// `topProducts` provides `vendor { name }`, so `shop` answers `Vendor.name` there from the data it
// already holds, though it marks the field external.
#[test]
fn an_external_field_inside_its_window_is_accepted() {
    let fixture = windows();
    check_root_fetch(&fixture, "shop", "{ topProducts { vendor { name } } }").unwrap();
}

// The window provides `name` under `vendor` only: under its sibling `maker`, `shop` holds no
// vendor's name to return.
#[test]
fn an_external_field_on_a_sibling_path_is_rejected() {
    let fixture = windows();
    assert_rejected(
        check_root_fetch(&fixture, "shop", "{ topProducts { maker { name } } }"),
        "`Vendor.name` is `@external` there",
    );
}

// `products` provides nothing, so `shop` cannot answer `Vendor.name` below it. A root fetch is
// held to what its subgraph resolves like an entity fetch is.
#[test]
fn an_external_field_outside_a_window_is_rejected() {
    let fixture = windows();
    assert_rejected(
        check_root_fetch(&fixture, "shop", "{ products { vendor { name } } }"),
        "`Vendor.name` is `@external` there",
    );
}

// The same fetch sent to `catalog`, which marks nothing external: a window is needed only for a
// field the subgraph itself marks.
#[test]
fn a_field_not_marked_external_is_accepted() {
    let fixture = windows();
    check_root_fetch(&fixture, "catalog", "{ products { vendor { name } } }").unwrap();
}

// `Product.vendor` provides `code` inside the window `topProducts` opened, and the two add up:
// `shop` answers both external fields of each top product's vendor.
#[test]
fn a_nested_window_adding_to_the_outer_one_is_accepted() {
    let fixture = windows();
    check_root_fetch(&fixture, "shop", "{ topProducts { vendor { name code } } }").unwrap();
}

// `featured` provides `maker { name }` for a gizmo, which is what is asked under `... on Gizmo`.
#[test]
fn a_conditioned_window_granting_its_type_is_accepted() {
    let fixture = windows();
    check_root_fetch(
        &fixture,
        "shop",
        "{ featured { ... on Gizmo { maker { name } } } }",
    )
    .unwrap();
}

// The window is conditioned on `Gizmo`, so for a featured widget `shop` holds no maker's name.
#[test]
fn a_conditioned_window_not_granting_another_member_is_rejected() {
    let fixture = windows();
    assert_rejected(
        check_root_fetch(
            &fixture,
            "shop",
            "{ featured { ... on Widget { maker { name } } } }",
        ),
        "`Vendor.name` is `@external` there",
    );
}

// `maker` spelled on `Item` is asked of a featured widget too, whose maker's name `shop` does not
// hold. Both members reach a `Vendor`, one with the window and one without, and they must not be
// merged into one vendor with the window.
#[test]
fn an_interface_field_reaching_an_ungranted_member_is_rejected() {
    let fixture = windows();
    assert_rejected(
        check_root_fetch(&fixture, "shop", "{ featured { maker { name } } }"),
        "`Vendor.name` is `@external` there",
    );
}

// `label` spelled on `Item` is asked of every member `items` can return, and `shop` marks
// `Gizmo.label` external with nothing provided.
#[test]
fn an_interface_field_reaching_an_external_member_is_rejected() {
    let fixture = windows();
    assert_rejected(
        check_root_fetch(&fixture, "shop", "{ items { label } }"),
        "`Gizmo.label` is `@external` there",
    );
}

// `shop` resolves `Widget.label`, and the fragment never runs for a gizmo.
#[test]
fn a_member_resolving_the_field_is_accepted() {
    let fixture = windows();
    check_root_fetch(&fixture, "shop", "{ items { ... on Widget { label } } }").unwrap();
}

// `... on Item` inside `... on Widget` still runs for widgets alone: a fragment applies to what
// the position holds, not to every type its condition names, so no gizmo is asked its `label`.
#[test]
fn an_interface_fragment_inside_a_member_is_accepted() {
    let fixture = windows();
    check_root_fetch(
        &fixture,
        "shop",
        "{ items { ... on Widget { ... on Item { label } } } }",
    )
    .unwrap();
}

//==================================================================================================
// What every fetch is held to

// The router has no subgraph to send the fetch to. The plan is read against `shop` and then
// renamed, since a written plan's operations are parsed in the schema of the subgraph it names.
#[test]
fn a_root_fetch_to_an_unknown_subgraph_is_rejected() {
    let fixture = windows();
    let operation = "{ topProducts { vendor { name } } }";
    let mut plan = fixture.written_plan(&root_fetch("shop", operation));
    let Some(TopLevelPlanNode::Fetch(fetch)) = &mut plan.node else {
        unreachable!("a root fetch")
    };
    fetch.subgraph_name = "nowhere".into();
    assert_rejected(
        fixture.check(&plan, operation),
        "fetch to unknown subgraph `nowhere`",
    );
}

// `__typename` is answered by every subgraph, here `ledger`, whose `Vendor` marks its one field
// external.
//
// This does not discriminate the exemption: `is_external` is never true of `__typename`, where
// the model's subgraph can mark every field external, `__typename` included.
#[test]
fn an_introspection_field_is_exempt() {
    let fixture = windows();
    check_selects_resolvable(
        &fixture,
        "ledger",
        "query($representations: [_Any!]!) \
         { __typename _entities(representations: $representations) \
         { __typename ... on Vendor { __typename } } }",
    )
    .unwrap();
}

// A fetch is read in its subgraph's own schema, where `_entities` is a field like any other: what
// is selected under it is asked of `shop` whether or not the fetch requires anything -- here
// nothing.
#[test]
fn a_field_under_entities_in_a_root_fetch_is_rejected() {
    let fixture = windows();
    assert_rejected(
        check_selects_resolvable(
            &fixture,
            "shop",
            "query($representations: [_Any!]!) \
             { _entities(representations: $representations) { ... on Vendor { name } } }",
        ),
        "`Vendor.name` is `@external` there",
    );
}

fn vendor_id_plan(fixture: &Fixture, subgraph: &str) -> QueryPlan {
    entity_plan(fixture, &["products", "vendor"], "Vendor", subgraph, "id")
}

const VENDOR_ID: &str = "{ products { vendor { id } } }";

// Selecting the key the fetch is keyed by asks `shop` to resolve it, which it does.
#[test]
fn a_key_selected_under_entities_is_accepted() {
    let fixture = windows();
    let plan = vendor_id_plan(&fixture, "shop");
    fixture.check(&plan, VENDOR_ID).unwrap();
}

// The same fetch sent to `ledger`, which marks `Vendor.id` external: the representation carries an
// `id`, but selecting it asks `ledger` to resolve a field it does not.
#[test]
fn an_external_key_selected_under_entities_is_rejected() {
    let fixture = windows();
    let plan = vendor_id_plan(&fixture, "ledger");
    assert_rejected(
        fixture.check(&plan, VENDOR_ID),
        "`Vendor.id` is `@external` there",
    );
}

/// `shop`, whose `Vendor` is a type extension with its key field `id` marked external, a fake
/// external, and whose `Brand` is marked external as a type; `catalog` resolves both.
fn resolution() -> Fixture {
    Fixture::new(include_str!("../testdata/subgraph_resolution.graphql"))
}

// Upstream's `a_key_selected_under_entities_is_accepted`, which is on a fake external.
// `shop` marks `Vendor.id` external on the extension its key is on, which the metadata reads as a
// field it resolves; a reading of `@external` that missed the fake would reject this.
#[test]
fn fake_external_key_selected_under_entities_is_accepted() {
    let fixture = resolution();
    let plan = entity_plan(
        &fixture,
        &["products", "@", "vendor"],
        "Vendor",
        "shop",
        "id",
    );
    fixture.check(&plan, VENDOR_ID).unwrap();
}

// Upstream's `an_external_key_selected_under_entities_is_rejected`.
// `shop` marks `Brand` external as a type, its key `id` included, so selecting a brand's key asks
// `shop` for a field it cannot resolve.
#[test]
fn key_of_a_type_marked_external_selected_under_entities_is_rejected() {
    let fixture = resolution();
    let plan = entity_plan(&fixture, &["products", "@", "brand"], "Brand", "shop", "id");
    assert_rejected(
        fixture.check(&plan, "{ products { brand { id } } }"),
        "`Brand.id` is `@external` there",
    );
}

// Upstream's `the_extracted_shop_reads_its_externals_as_written`.
// The `shop` extracted from the supergraph keeps the fake external on `Vendor.id`, which it
// resolves; `Brand.id` is external through its type, and `Vendor.name` external.
#[test]
fn extracted_subgraph_reads_its_externals_as_written() {
    let fixture = resolution();
    let shop = Subgraph::new(&fixture.subgraphs["shop"]).expect("a federation subgraph");
    let metadata = fixture.subgraphs["shop"]
        .subgraph_metadata()
        .expect("subgraph metadata");
    let vendor_id = FieldDefinitionPosition::Object(ObjectFieldDefinitionPosition {
        type_name: name!("Vendor"),
        field_name: name!("id"),
    });
    assert!(metadata.is_field_fake_external(&vendor_id));
    assert!(!shop.is_external(&name!("Vendor"), &name!("id")));
    assert!(shop.is_external(&name!("Brand"), &name!("id")));
    assert!(shop.is_external(&name!("Vendor"), &name!("name")));
}

fn top_vendor_plan(fixture: &Fixture, leaf: &str) -> QueryPlan {
    entity_plan(
        fixture,
        &["topProducts"],
        "Product",
        "shop",
        &format!("vendor {{ {leaf} }}"),
    )
}

// `topProducts` provides `vendor { name }`, but this fetch reaches each product through
// `_entities`, not through `topProducts`, so `shop` holds no vendor's name for it.
#[test]
fn an_external_field_granted_above_an_entity_fetch_is_rejected() {
    let fixture = windows();
    let plan = top_vendor_plan(&fixture, "name");
    assert_rejected(
        fixture.check(&plan, "{ topProducts { vendor { name } } }"),
        "`Vendor.name` is `@external` there",
    );
}

// `Product.vendor` provides `code` itself, inside the entity fetch, so `shop` answers it.
#[test]
fn a_window_opened_inside_an_entity_fetch_is_accepted() {
    let fixture = windows();
    let plan = top_vendor_plan(&fixture, "code");
    fixture
        .check(&plan, "{ topProducts { vendor { code } } }")
        .unwrap();
}

/// `root` is a `Recursive`, an interface with four implementations whose `next` returns another
/// `Recursive`. Every member marks `leaf` external, and every `next` provides it for every member.
const RECURSIVE: &str = r#"
extend schema @link(url: "https://specs.apollo.dev/federation/v2.3", import: ["@external", "@provides"])

type Query {
  root: Recursive
}

interface Recursive {
  next: Recursive
  leaf: String
}

type First implements Recursive {
  next: Recursive @provides(fields: "... on First { leaf } ... on Second { leaf } ... on Third { leaf } ... on Fourth { leaf }")
  leaf: String @external
}

type Second implements Recursive {
  next: Recursive @provides(fields: "... on First { leaf } ... on Second { leaf } ... on Third { leaf } ... on Fourth { leaf }")
  leaf: String @external
}

type Third implements Recursive {
  next: Recursive @provides(fields: "... on First { leaf } ... on Second { leaf } ... on Third { leaf } ... on Fourth { leaf }")
  leaf: String @external
}

type Fourth implements Recursive {
  next: Recursive @provides(fields: "... on First { leaf } ... on Second { leaf } ... on Third { leaf } ... on Fourth { leaf }")
  leaf: String @external
}
"#;

// A chain of 64 `next` through the interface, each opening the same window. The walk pools the
// runtime types that share a window, so each level is asked once; a descent per runtime path would
// be 4^64 of them and never finish.
#[test]
fn a_deep_interface_chain_is_walked_once() {
    let schema = crate::subgraph::typestate::Subgraph::parse("recursive", "none", RECURSIVE)
        .expect("parses")
        .expand_links()
        .expect("expands")
        .validate()
        .expect("valid subgraph")
        .validated_schema()
        .clone();
    let chain = (0..64).fold("leaf".to_string(), |inner, _| format!("next {{ {inner} }}"));
    let (root_type, selections) = selections(&schema, &format!("{{ root {{ {chain} }} }}"));
    check_fetch_selects_resolvable("recursive", &schema, &root_type, &selections).unwrap();
    // The collector of contextual uses walks the same chain, pooling the runtime types below a
    // field, and completes on it. This shows completion on this input, not a growth law.
    let subgraph = Subgraph::new(&schema).expect("a federation subgraph");
    let uses = context_uses(&subgraph, &schema, &root_type, &selections).unwrap();
    assert!(uses.is_empty());
}

//==================================================================================================
// A fetch's fragments, read as its subgraph applies them

const EVERY_VALUE: &str = "{ item { ... on I { value } } }";

// `source` returns a `B` for `item` without running `... on I`, so nothing supplies a `B`'s
// `value`. Read in the supergraph, where `B` implements `I`, the fragment was credited with it.
#[test]
fn a_fragment_the_subgraph_does_not_apply_is_rejected() {
    let fixture = membership();
    let plan = fixture.written_plan(&root_fetch("source", EVERY_VALUE));
    assert_rejected(
        fixture.check(&plan, EVERY_VALUE),
        "does not select `value` on B",
    );
}

// A fragment per member supplies `value` for both.
#[test]
fn a_fragment_per_member_is_accepted() {
    let fixture = membership();
    let plan = fixture.written_plan(&root_fetch(
        "source",
        "{ item { ... on A { value } ... on B { value } } }",
    ));
    fixture.check(&plan, EVERY_VALUE).unwrap();
}

// `other` declares `B` as implementing `I` too, so there the fragment runs for both.
#[test]
fn a_fragment_both_schemas_apply_alike_is_accepted() {
    let fixture = membership();
    check_root_fetch(&fixture, "other", EVERY_VALUE).unwrap();
}

// `... on I` is narrowed to the object types `source` admits at `I`, and kept as written where
// the subgraph admits what the supergraph does.
#[test]
fn a_type_condition_is_narrowed_to_its_subgraph() {
    let fixture = membership();
    let narrowed = |subgraph: &str| {
        let (_, selections) = selections(&fixture.subgraphs[subgraph], EVERY_VALUE);
        let [Selection::Field(item)] = selections.as_slice() else {
            unreachable!("one root field")
        };
        narrow_type_conditions(
            &fixture.supergraph_schema,
            &fixture.subgraphs[subgraph],
            &item.selection_set.selections,
        )
        .iter()
        .map(|selection| selection.serialize().no_indent().to_string())
        .collect::<Vec<_>>()
    };
    assert_eq!(narrowed("source"), ["... on A { value }"]);
    assert_eq!(narrowed("other"), ["... on I { value }"]);
}

//==================================================================================================
// A mutation root

/// `orders` serves the mutation `buy` and a `Sub`'s `vendor`, but marks `Vendor.orderId`
/// `@external`; `vendors` resolves it.
fn external_mutation() -> Fixture {
    Fixture::new(include_str!("../testdata/external_mutation.graphql"))
}

const VENDOR_ORDER_ID: &str = "mutation { buy { sub { vendor { orderId } } } }";

/// A fetch to `subgraph` running the mutation `operation`.
fn mutation_fetch(subgraph: &str, operation: &str) -> String {
    root_fetch(subgraph, operation).replace(
        r#""operation_kind": "query""#,
        r#""operation_kind": "mutation""#,
    )
}

// The planner's plan for the corpus shape: `orders` is asked for `orderId`, which it only reads as
// a `@requires` input. The resolvability check reads a mutation root as it reads a query root.
#[test]
fn an_external_field_under_a_mutation_root_is_rejected() {
    let fixture = external_mutation();
    assert_rejected(
        fixture.check(
            &fixture.written_plan(&mutation_fetch("orders", VENDOR_ORDER_ID)),
            VENDOR_ORDER_ID,
        ),
        "`Vendor.orderId` is `@external` there",
    );
}

// The plan the planner should write: `orders` runs the mutation for each `Sub`'s key, and
// `vendors` resolves the `orderId`.
#[test]
fn an_external_field_fetched_from_its_resolver_is_accepted() {
    let fixture = external_mutation();
    let plan = fixture.written_plan(&format!(
        r#"{{ "Sequence": {{ "nodes": [
            {},
            {{ "Flatten": {{
                "path": [{{ "Key": ["buy", null] }}, {{ "Key": ["sub", null] }}],
                "node": {{ "Fetch": {{
                    "subgraph_name": "vendors",
                    "variable_usages": [],
                    "requires": [{{ "kind": "InlineFragment", "typeCondition": "Sub", "selections": [
                        {{ "kind": "Field", "name": "__typename" }},
                        {{ "kind": "Field", "name": "id" }}
                    ] }}],
                    "operation_document": "query($representations: [_Any!]!) {{ _entities(representations: $representations) {{ ... on Sub {{ vendor {{ orderId }} }} }} }}",
                    "operation_kind": "query",
                    "input_rewrites": [],
                    "output_rewrites": [],
                    "context_rewrites": []
                }} }}
            }} }}
        ] }} }}"#,
        mutation_fetch("orders", "mutation { buy { sub { __typename id } } }"),
    ));
    fixture.check(&plan, VENDOR_ORDER_ID).unwrap();
}
