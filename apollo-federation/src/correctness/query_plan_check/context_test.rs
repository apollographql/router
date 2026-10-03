//! Port of `Apollo/Tests/QueryPlanChecker.lean`.
//!
//! `@context` is the part of plan correctness with the most moving parts, and every case here is
//! one a plan the router executes correctly was once rejected over, or one that must stay
//! rejected. The supergraph carries an interface ancestor: `Query.t` is an `I` implemented by `A`
//! and `B`, which both declare `prop` and `u`, and `Query.concrete` is a `T`, which has the same
//! fields and no implementations to choose between.
//!
//! Plans as a whole are not covered here, only the fetch-level machinery.

use apollo_compiler::ExecutableDocument;
use apollo_compiler::executable;
use apollo_compiler::executable::Selection;
use apollo_compiler::name;
use apollo_compiler::schema::Schema;

use super::*;
use crate::query_plan::FetchDataKeyRenamer;
use crate::query_plan::serializable_document::SerializableDocument;

const SCHEMA_STR: &str = r#"
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

fn schema() -> ValidFederationSchema {
    let schema = Schema::parse_and_validate(SCHEMA_STR, "schema.graphql").unwrap();
    ValidFederationSchema::new(schema).unwrap()
}

/// The selections of an operation against the test schema, which is how every fixture here is
/// written: the checker reads `available` rooted at the query type.
fn selections(schema: &ValidFederationSchema, operation: &str) -> Vec<Selection> {
    ExecutableDocument::parse_and_validate(schema.schema(), operation, "op.graphql")
        .unwrap()
        .operations
        .get(None)
        .unwrap()
        .selection_set
        .selections
        .clone()
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
// Dropping the synthetic arguments
//==================================================================================================

/// The fetch resolving `u` passes the contextual value as an argument the client never wrote, and
/// a rewrite binding that variable.
fn context_selections(schema: &ValidFederationSchema) -> Vec<Selection> {
    selections(
        schema,
        r#"query($contextual: String) { t { ... on A { u(ctx: $contextual) { id } } } }"#,
    )
}

#[test]
fn context_argument_is_dropped() {
    let schema = schema();
    let stripped = remove_context_arguments(&[name!("contextual")], context_selections(&schema));
    assert_eq!(
        stripped
            .iter()
            .map(|s| s.serialize().no_indent().to_string())
            .collect::<Vec<_>>(),
        vec!["t { ... on A { u { id } } }"]
    );
}

/// An argument the client did write stays, the variable not being one a rewrite binds.
#[test]
fn other_arguments_stay() {
    let schema = schema();
    let kept = remove_context_arguments(&[], context_selections(&schema));
    assert_eq!(
        kept.iter()
            .map(|s| s.serialize().no_indent().to_string())
            .collect::<Vec<_>>(),
        vec!["t { ... on A { u(ctx: $contextual) { id } } }"]
    );
}

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

/// What a fetch at `path` needs of `available`, as the checker asks it.
fn context_met(
    path: &[FetchDataPathElement],
    condition: &[BooleanLiteral],
    available: &str,
    rewrites: Vec<Arc<FetchDataRewrite>>,
) -> bool {
    let schema = schema();
    let available = selections(&schema, available);
    let fetch = FetchNode {
        subgraph_name: "s".into(),
        id: None,
        variable_usages: Vec::new(),
        requires: Vec::new(),
        operation_document: SerializableDocument::from_string(""),
        operation_name: None,
        operation_kind: executable::OperationType::Query,
        input_rewrites: Default::default(),
        output_rewrites: Vec::new(),
        context_rewrites: rewrites,
    };
    check_context_rewrites(
        &schema,
        &name!("Query"),
        path,
        condition,
        &available,
        &fetch,
    )
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

/// The value is guarded past the levels the two paths share, and nothing about the fetch says what
/// that object is.
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
