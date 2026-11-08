//! Families of plans growing in depth or width.
//!
//! Each family's operations and plans grow with a size the schema does not: a copy's possible
//! runtime types depend on its parent's, so the configurations the client could see multiply with
//! depth while the plan's syntax grows linearly. Each verdict is checked at small sizes, beside a
//! rejected neighbour. The families the check handles without enumerating configurations are also
//! checked at a size no enumeration could finish, as a structural growth guard: there is no timing
//! assertion, the test simply does not end if the check starts enumerating.
//!
//! The complementary-ancestor and independent-pair families have no such guard yet: the check's
//! work on their accepted plans doubles with each level or pair.

use serde_json::Value;
use serde_json::json;

use super::CorrectnessError;
use super::Fixture;

const INCOMPLETE: &str = "query plan does not fetch everything the operation requests";
const UNFETCHED_KEY: &str = "reads a key the plan has not fetched";

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

fn check(fixture: &Fixture, plan: Value, operation: &str) -> Result<(), CorrectnessError> {
    fixture.check(&fixture.written_plan(&plan.to_string()), operation)
}

/// Expects a rejection for `reason`, naming each of `details`.
#[track_caller]
fn expect_rejected(result: Result<(), CorrectnessError>, reason: &str, details: &[&str]) {
    let error = result
        .expect_err("expected the check to reject this plan")
        .to_string();
    assert!(
        error.contains(reason) && details.iter().all(|detail| error.contains(detail)),
        "{error}"
    );
}

/// `{ root { next { … { inner } } } }`, `depth` levels of `next`.
fn nexts(depth: usize, inner: &str) -> String {
    format!(
        "{{ root {{ {}{inner}{} }} }}",
        "next { ".repeat(depth),
        " }".repeat(depth)
    )
}

//==================================================================================================
// A covariant chain

/// An `A`'s `next` is an `I` and a `B`'s an `A`, so what a level can be depends on the level above.
fn covariant_chain() -> Fixture {
    Fixture::new(include_str!("../testdata/covariant_chain.graphql"))
}

/// The chain fetched to `depth`, its last level's leaf under `leaf`.
fn check_covariant(fixture: &Fixture, depth: usize, leaf: &str) -> Result<(), CorrectnessError> {
    check(
        fixture,
        fetch("chain", &nexts(depth, leaf)),
        &nexts(depth, "leaf"),
    )
}

// Every level reached can be a `B`, below an `A`: the `leaf` fetched at the last level is served,
// and fetched there under `A` alone it is missing where that level is a `B`. Upstream's
// `a_covariant_chain_owes_its_leaf_at_every_reachable_type`.
#[test]
fn covariant_chain_owes_its_leaf_at_every_reachable_type() {
    let fixture = covariant_chain();
    for depth in [1, 2, 5] {
        check_covariant(&fixture, depth, "leaf").unwrap();
        expect_rejected(
            check_covariant(&fixture, depth, "... on A { leaf }"),
            INCOMPLETE,
            &["does not select `leaf` on B"],
        );
    }
}

// The growth guard: at depth 64 the levels' types have 2^64 combinations.
#[test]
fn deep_covariant_chain_is_checked_without_enumerating_its_levels() {
    let fixture = covariant_chain();
    check_covariant(&fixture, 64, "leaf").unwrap();
    expect_rejected(
        check_covariant(&fixture, 64, "... on A { leaf }"),
        INCOMPLETE,
        &["does not select `leaf` on B"],
    );
}

//==================================================================================================
// Complementary contributions over independent ancestors

/// Every level of the chain is an `A` or a `B`, independently, ending in a `leaf`.
fn complementary_ancestors() -> Fixture {
    Fixture::new(include_str!("../testdata/complementary_ancestors.graphql"))
}

/// What is selected at `level` of `depth` and below: the leaf's `f{level}` under each type of
/// `types(level)`, then the next level.
fn complementary_level(
    level: usize,
    depth: usize,
    types: &dyn Fn(usize) -> Vec<&'static str>,
) -> String {
    let below = depth - 1 - level;
    let leaf = format!(
        "{}leaf {{ f{level}: f }}{}",
        "next { ".repeat(below),
        " }".repeat(below)
    );
    let mut selections: Vec<String> = types(level)
        .into_iter()
        .map(|type_name| format!("... on {type_name} {{ {leaf} }}"))
        .collect();
    if level + 1 < depth {
        selections.push(format!(
            "next {{ {} }}",
            complementary_level(level + 1, depth, types)
        ));
    } else {
        selections.push("leaf { __typename }".to_string());
    }
    selections.join(" ")
}

/// `depth` levels, each `f{level}` fetched under both of its level's types, except `f{missing}`
/// under `A` alone.
fn check_complementary(depth: usize, missing: Option<usize>) -> Result<(), CorrectnessError> {
    let types = |level: usize| {
        if Some(level) == missing {
            vec!["A"]
        } else {
            vec!["A", "B"]
        }
    };
    let fields: Vec<String> = (0..depth).map(|level| format!("f{level}: f")).collect();
    check(
        &complementary_ancestors(),
        fetch(
            "chain",
            &format!("{{ root {{ {} }} }}", complementary_level(0, depth, &types)),
        ),
        &nexts(depth - 1, &format!("leaf {{ {} }}", fields.join(" "))),
    )
}

// Each `f{level}` is fetched by two contributions, one under each type its level can have, and
// between them they are there whatever every level is; without one, `f{level}` is missing where
// that level is a `B`. Upstream's `complementary_contributions_cover_every_ancestor_type`.
#[test]
fn complementary_contributions_cover_every_ancestor_type() {
    for depth in [1, 2, 5] {
        check_complementary(depth, None).unwrap();
        let missing = depth / 2;
        expect_rejected(
            check_complementary(depth, Some(missing)),
            INCOMPLETE,
            &[&format!("does not select `f{missing}` on Leaf")],
        );
    }
}

//==================================================================================================
// Independent pairs of levels

/// Each pair of levels is an `A` over a `C` or a `B` over a `D`, independently of the others.
fn independent_pairs() -> Fixture {
    Fixture::new(include_str!("../testdata/independent_pairs.graphql"))
}

/// What is selected at `level` of `2 * pairs` and below: a pair's leaf field under `under(level)`,
/// if any, then the next level, and the last level's `leaf`.
fn pair_level(level: usize, pairs: usize, under: &dyn Fn(usize) -> Option<&'static str>) -> String {
    if level == 2 * pairs {
        return "leaf { __typename }".to_string();
    }
    let mut selections = Vec::new();
    if let Some(type_name) = under(level) {
        let below = 2 * pairs - level;
        selections.push(format!(
            "... on {type_name} {{ {}leaf {{ f{}: f }}{} }}",
            "next { ".repeat(below),
            level / 2,
            " }".repeat(below)
        ));
    }
    selections.push(format!(
        "next {{ {} }}",
        pair_level(level + 1, pairs, under)
    ));
    selections.join(" ")
}

/// Each pair's leaf field asked under the pair's `A` and fetched under its `C`, or under its `D`
/// for pair `changed`.
fn check_pairs(pairs: usize, changed: Option<usize>) -> Result<(), CorrectnessError> {
    let fetched = |level: usize| {
        (!level.is_multiple_of(2)).then(|| if Some(level / 2) == changed { "D" } else { "C" })
    };
    let asked = |level: usize| level.is_multiple_of(2).then_some("A");
    check(
        &independent_pairs(),
        fetch(
            "chain",
            &format!("{{ root {{ {} }} }}", pair_level(0, pairs, &fetched)),
        ),
        &format!("{{ root {{ {} }} }}", pair_level(0, pairs, &asked)),
    )
}

// A pair's field asked under its `A` and fetched under its `C` is there wherever it is asked;
// fetched under one pair's `D` instead, it is missing where that pair is an `A` over a `C`.
// Upstream's `independent_pairs_each_supply_their_own_field`.
#[test]
fn independent_pairs_each_supply_their_own_field() {
    for pairs in [1, 2, 4] {
        check_pairs(pairs, None).unwrap();
        let changed = pairs / 2;
        expect_rejected(
            check_pairs(pairs, Some(changed)),
            INCOMPLETE,
            &[&format!("does not select `f{changed}` on Leaf")],
        );
    }
}

//==================================================================================================
// A field under each member's fragment

/// `root` is an `Outer`, the union of `O1` to `O32`, each holding a `leaf` that `L1` to `L16`
/// implement.
fn member_fragments() -> Fixture {
    Fixture::new(include_str!("../testdata/member_fragments.graphql"))
}

/// `leaf { v }` under each of the first `members` members, and the `leaf`'s `__typename` alone
/// under `missing`'s.
fn member_selections(members: usize, missing: Option<usize>) -> String {
    let selections: Vec<String> = (1..=members)
        .map(|member| {
            let leaf = if Some(member) == missing {
                "leaf { __typename }"
            } else {
                "leaf { v }"
            };
            format!("... on O{member} {{ {leaf} }}")
        })
        .collect();
    format!("{{ root {{ {} }} }}", selections.join(" "))
}

fn check_member_fragments(members: usize, missing: Option<usize>) -> Result<(), CorrectnessError> {
    check(
        &member_fragments(),
        fetch("chain", &member_selections(members, missing)),
        &member_selections(members, None),
    )
}

// The fetch writes one `leaf` copy and its `v` under every member it selects, at every
// implementation the copy can be; left out under one member, `v` is missing where `root` is that
// member. Upstream's `a_field_selected_under_each_members_fragment_is_supplied_under_each`, with
// every member of the union as a growth guard.
#[test]
fn field_selected_under_each_members_fragment_is_supplied_under_each() {
    for members in [1, 2, 8, 32] {
        check_member_fragments(members, None).unwrap();
        let missing = members / 2 + 1;
        expect_rejected(
            check_member_fragments(members, Some(missing)),
            INCOMPLETE,
            &[
                &format!("over runtime types: {{O{missing}}}"),
                "does not select `v` on L1",
            ],
        );
    }
}

//==================================================================================================
// Entity fetches at an interface's members

/// `base` serves `items8`, `items16` and `items32`, lists of interfaces implemented by `E1` to
/// `E8`, `E16` or `E32`; `extra` resolves each one's `x1` to `x8`.
fn entity_members() -> Fixture {
    Fixture::new(include_str!("../testdata/entity_members.graphql"))
}

/// Each of `items{members}` from `base` with its `id`, and its `__typename` when `observed`, then
/// `fetches` entity fetches from `extra` at the items, their path conditioned on every member, the
/// `j`th asking each member's `x{j}`; the client asks every member's `x1` to `x{fetches}`.
fn check_entity_members(
    members: usize,
    fetches: usize,
    observed: bool,
) -> Result<(), CorrectnessError> {
    let typename = if observed { "__typename " } else { "" };
    let names: Vec<String> = (1..=members).map(|member| format!("E{member}")).collect();
    let entries: Vec<Value> = names
        .iter()
        .map(|name| {
            json!({
                "kind": "InlineFragment",
                "typeCondition": name,
                "selections": [
                    { "kind": "Field", "name": "__typename" },
                    { "kind": "Field", "name": "id" }
                ]
            })
        })
        .collect();
    let mut nodes = vec![fetch(
        "base",
        &format!("{{ items{members} {{ {typename}id }} }}"),
    )];
    for field in 1..=fetches {
        let cases: Vec<String> = names
            .iter()
            .map(|name| format!("... on {name} {{ x{field} }}"))
            .collect();
        let mut entity_fetch = fetch(
            "extra",
            &format!(
                "query($representations: [_Any!]!) \
                 {{ _entities(representations: $representations) {{ {} }} }}",
                cases.join(" ")
            ),
        );
        entity_fetch["Fetch"]["requires"] = json!(entries);
        nodes.push(json!({ "Flatten": {
            "path": [{ "Key": [format!("items{members}"), null] }, { "AnyIndex": names }],
            "node": entity_fetch
        } }));
    }
    let fields: Vec<String> = (1..=fetches).map(|field| format!("x{field}")).collect();
    let asked: Vec<String> = names
        .iter()
        .map(|name| format!("... on {name} {{ {} }}", fields.join(" ")))
        .collect();
    check(
        &entity_members(),
        json!({ "Sequence": { "nodes": nodes } }),
        &format!("{{ items{members} {{ {} }} }}", asked.join(" ")),
    )
}

// The planner's shape for an interface's entities: fetched with the items' `__typename`, the plan
// is complete; without it, the entries and the path's member conditions read a `__typename` no
// fetch wrote. Upstream's `entity_fetches_at_an_interfaces_members_read_their_typename`, with the
// widest interface as a growth guard.
#[test]
fn entity_fetches_at_an_interfaces_members_read_their_typename() {
    for (members, fetches) in [(8, 1), (16, 2), (32, 2)] {
        check_entity_members(members, fetches, true).unwrap();
        expect_rejected(
            check_entity_members(members, fetches, false),
            UNFETCHED_KEY,
            &["`__typename` is not read on E1"],
        );
    }
}
