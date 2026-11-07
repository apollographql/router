//! Port of `Apollo/Tests/QueryPlanChecker.lean`.
//!
//! `@context` is the part of plan correctness with the most moving parts, and every case here is
//! one a plan the router executes correctly was once rejected over, or one that must stay
//! rejected. The supergraph carries an interface ancestor: `Query.t` is an `I` implemented by `A`
//! and `B`, which both declare `prop` and `u`, and `Query.concrete` is a `T`, which has the same
//! fields and no implementations to choose between.
//!
//! Plans as a whole are not covered here, only the fetch-level machinery: what a fetch reads is
//! what one root fetch before it produced, from a subgraph whose schema is this one.

use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable;
use apollo_compiler::name;

use super::super::super::query_compare::conditions::BooleanLiteral;
use super::super::Site;
use super::super::coverage::Supplies;
use super::super::hypothesis::Hypothesis;
use super::super::region::Algebra;
use super::super::region::boolean;
use super::super::response::Mount;
use super::super::response::Production;
use super::super::response::Response;
use super::super::sharing::Sharing;
use super::super::test_support::recursive_chain;
use super::super::test_support::recursive_subgraph;
use super::super::test_support::selections;
use super::*;
use crate::query_plan::FetchDataKeyRenamer;
use crate::query_plan::requires_selection;
use crate::query_plan::serializable_document::SerializableDocument;

const SCHEMA_STR: &str = r#"
    extend schema @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@key"])

    type Query {
        t: I!
        media: I
        concrete: T!
    }

    interface I {
        prop: String
        u: U
    }

    type A implements I {
        prop: String
        u(ctx: String): U
    }

    type B implements I {
        prop: String
        u(ctx: String): U
    }

    type T {
        prop: String
        u(ctx: String): U
    }

    type U {
        id: ID
    }
"#;

/// The schema, as the subgraph the earlier fetch is sent to; it is the supergraph too.
fn subgraph() -> crate::subgraph::typestate::Subgraph<crate::subgraph::typestate::Validated> {
    crate::subgraph::typestate::Subgraph::parse("before", "http://before", SCHEMA_STR)
        .expect("valid subgraph")
        .expand_links()
        .expect("links expand")
        .validate()
        .expect("valid subgraph schema")
}

fn key(name: Name) -> FetchDataPathElement {
    FetchDataPathElement::Key(name, Default::default())
}

fn renamer(path: Vec<FetchDataPathElement>) -> Arc<FetchDataRewrite> {
    Arc::new(FetchDataRewrite::KeyRenamer(FetchDataKeyRenamer {
        path,
        rename_key_to: name!("contextual"),
    }))
}

/// A rewrite climbing to the ancestor and reading `prop` off data of one runtime type.
fn rewrite_on(type_name: Name) -> Arc<FetchDataRewrite> {
    renamer(vec![
        FetchDataPathElement::Parent,
        FetchDataPathElement::TypenameEquals(type_name),
        key(name!("prop")),
    ])
}

//==================================================================================================
// The variables a fetch's rewrites bind
//==================================================================================================

#[test]
fn context_variables_are_the_renamers_targets() {
    let rewrites = vec![rewrite_on(name!("A")), rewrite_on(name!("B"))];
    assert_eq!(
        context_variables(&rewrites),
        vec![name!("contextual"), name!("contextual")]
    );
}

//==================================================================================================
// Resolving a rewrite's path
//==================================================================================================

fn fetch_path() -> Vec<FetchDataPathElement> {
    vec![key(name!("t")), key(name!("u"))]
}

#[test]
fn one_parent_climbs_to_the_ancestor() {
    let merged = merge_context_path(
        &fetch_path(),
        &[FetchDataPathElement::Parent, key(name!("prop"))],
    );
    assert_eq!(merged, Some(vec![key(name!("t")), key(name!("prop"))]));
}

#[test]
fn indices_do_not_name_a_level() {
    let path = vec![
        key(name!("t")),
        key(name!("u")),
        FetchDataPathElement::AnyIndex(Default::default()),
    ];
    let merged = merge_context_path(&path, &[FetchDataPathElement::Parent, key(name!("prop"))]);
    assert_eq!(merged, Some(vec![key(name!("t")), key(name!("prop"))]));
}

#[test]
fn climbing_past_the_root_fails() {
    let merged = merge_context_path(
        &fetch_path(),
        &[
            FetchDataPathElement::Parent,
            FetchDataPathElement::Parent,
            FetchDataPathElement::Parent,
            key(name!("prop")),
        ],
    );
    assert_eq!(merged, None);
}

#[test]
fn no_climb_stays_under_the_fetch() {
    let merged = merge_context_path(&fetch_path(), &[key(name!("id"))]);
    assert_eq!(
        merged,
        Some(vec![key(name!("t")), key(name!("u")), key(name!("id"))])
    );
}

//==================================================================================================
// Reading the value where the fetch runs
//==================================================================================================

/// What a fetch at `path` needs of `available`, as the checker asks it outside a deferred block,
/// where a fetch can read everything placed before it. `available` is what a root fetch before it
/// selected. Every path here ends at a `U`, so the fetch is sent the `U` entities there: its
/// position is narrowed to the types its `requires` entries are written at.
fn context_met(
    path: &[FetchDataPathElement],
    condition: &[BooleanLiteral],
    available: &str,
    rewrites: Vec<Arc<FetchDataRewrite>>,
) -> bool {
    let subgraph = subgraph();
    let schema = subgraph.validated_schema();
    let view = SchemaView::new(schema).expect("schema index");
    let declarations = Subgraph::new(schema).expect("subgraph metadata");
    let subgraphs: IndexMap<Arc<str>, ValidFederationSchema> =
        [("before".into(), schema.clone())].into_iter().collect();
    let algebra = Algebra::new(schema);
    let sharing = Sharing::new(&subgraphs).expect("subgraph metadata");
    let mut response = Response::new(name!("Query"));
    let producer = response.new_producer("before".into());
    let mut added = response
        .produce_fetch(
            &Production {
                subgraph: &declarations,
                subgraph_name: "before",
                sharing: &sharing,
                supergraph: &view,
                algebra: &algebra,
                producer,
            },
            Mount {
                copy: CopyId::ROOT,
                base: everything(),
                cases: vec![(name!("Query"), algebra.objects([&name!("Query")]))],
            },
            &selections(schema, available),
            &[],
        )
        .expect("produced");
    added.sort();
    let fetch = FetchNode {
        subgraph_name: "s".into(),
        id: None,
        variable_usages: Vec::new(),
        requires: vec![requires_selection::Selection::InlineFragment(
            requires_selection::InlineFragment {
                type_condition: Some(name!("U")),
                conditions: Vec::new(),
                selections: Vec::new(),
            },
        )],
        operation_document: SerializableDocument::from_string(""),
        operation_name: None,
        operation_kind: executable::OperationType::Query,
        input_rewrites: Default::default(),
        output_rewrites: Vec::new(),
        context_rewrites: rewrites,
        protocol: Default::default(),
    };
    let state = response.view(&added);
    let Some(position) = response.position(&algebra, path) else {
        return true;
    };
    let Some(region) = algebra
        .clause(condition.iter().map(boolean))
        .and_then(|condition| algebra.conjoin(&position.region, &condition))
        .and_then(|region| {
            let literal =
                response.type_literal(&algebra, position.copy, algebra.objects([&name!("U")]))?;
            algebra.conjoin(&region, &literal)
        })
    else {
        return true;
    };
    let hypothesis = Hypothesis::new(&algebra, state);
    check_context_rewrites(&Site {
        fetch: &fetch,
        path,
        schema: &view,
        position: &position,
        region,
        hypothesis: &hypothesis,
        readable: state,
        supplies: Supplies::new(state),
    })
    .is_ok()
}

const FLAT: &str = "{ concrete { prop u { id } } }";
const SPLIT: &str = "{ t { ... on A { prop u { id } } ... on B { prop u { id } } } }";
const UNSPLIT: &str = "{ t { prop u { id } } }";

fn concrete_path() -> Vec<FetchDataPathElement> {
    vec![key(name!("concrete")), key(name!("u"))]
}

/// A `TypenameEquals` on the only type the position can have narrows nothing, so it must not
/// reject a fetch position that carries no fragment of its own.
#[test]
fn vacuous_type_condition_is_met() {
    assert!(context_met(
        &concrete_path(),
        &[],
        FLAT,
        vec![rewrite_on(name!("T"))]
    ));
}

/// One rewrite per runtime type, and a fetch position the query split the same way.
#[test]
fn split_position_is_met() {
    assert!(context_met(
        &fetch_path(),
        &[],
        SPLIT,
        vec![rewrite_on(name!("A")), rewrite_on(name!("B"))]
    ));
}

/// The same rewrites where the query selects the interface's own fields, so nothing splits the
/// fetch position: no single rewrite covers it, and only taking the runtime types one at a time
/// shows that between them they do.
#[test]
fn unsplit_position_is_met() {
    assert!(context_met(
        &fetch_path(),
        &[],
        UNSPLIT,
        vec![rewrite_on(name!("A")), rewrite_on(name!("B"))]
    ));
}

/// Drop one of them and the runtime types it covered have no value.
#[test]
fn missing_runtime_type_is_not_met() {
    assert!(!context_met(
        &fetch_path(),
        &[],
        UNSPLIT,
        vec![rewrite_on(name!("A"))]
    ));
}

/// A value the plan never fetched.
#[test]
fn absent_value_is_not_met() {
    assert!(!context_met(
        &fetch_path(),
        &[],
        "{ t { u { id } } }",
        vec![rewrite_on(name!("A")), rewrite_on(name!("B"))]
    ));
}

/// The value is fetched only where `media`'s runtime type is `A`, past the levels the two paths
/// share, and `media` can be a `B`, which holds none.
#[test]
fn narrowing_past_the_shared_levels_is_not_met() {
    let rewrite = renamer(vec![
        FetchDataPathElement::Parent,
        FetchDataPathElement::Parent,
        key(name!("media")),
        key(name!("prop")),
    ]);
    assert!(!context_met(
        &fetch_path(),
        &[],
        "{ t { prop u { id } } media { ... on A { prop } } }",
        vec![rewrite]
    ));
}

//==================================================================================================
// Reading it whenever the fetch runs
//==================================================================================================

const GUARDED: &str = r#"query($v: Boolean!) { concrete { prop @include(if: $v) u { id } } }"#;

/// A value fetched only under `@include(if: $v)` does not serve a fetch that runs without it.
#[test]
fn guarded_value_is_not_met_unconditionally() {
    assert!(!context_met(
        &concrete_path(),
        &[],
        GUARDED,
        vec![rewrite_on(name!("T"))]
    ));
}

/// It does serve a fetch that runs under the same literal.
#[test]
fn guarded_value_is_met_under_the_same_literal() {
    assert!(context_met(
        &concrete_path(),
        &[BooleanLiteral::Positive(name!("v"))],
        GUARDED,
        vec![rewrite_on(name!("T"))]
    ));
}

/// The fetch's own path is guarded too, and that guard is the fetch's to assume: the two cancel.
#[test]
fn shared_guard_cancels() {
    assert!(context_met(
        &concrete_path(),
        &[],
        r#"query($v: Boolean!) { concrete @include(if: $v) { prop u { id } } }"#,
        vec![rewrite_on(name!("T"))]
    ));
}

/// The chain the resolution walk is tested on, for the collector of contextual uses: it completes,
/// the runtime types below a field being pooled, and finds no argument taken from a context. It
/// shows completion on this input, not a growth law.
///
/// The model's `deepInterfaceChainIsCollectedOnce`.
#[test]
fn a_deep_interface_chain_is_collected_once() {
    let subgraph = recursive_subgraph();
    let schema = subgraph.validated_schema();
    let document = recursive_chain(schema, 64);
    let operation = document.operations.get(None).expect("one operation");
    let subgraph = Subgraph::new(schema).expect("subgraph metadata");
    let uses = context_uses(&subgraph, &operation.selection_set).expect("uses collected");
    assert!(uses.is_empty());
}
