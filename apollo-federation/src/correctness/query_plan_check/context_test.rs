//! Port of `Apollo/Tests/QueryPlanChecker.lean`.
//!
//! `@context` is the part of plan correctness with the most moving parts, and every case here is
//! one a plan the router executes correctly was once rejected over, or one that must stay
//! rejected. The supergraph carries an interface ancestor: `Query.t` is an `I` implemented by `A`
//! and `B`, which both declare `prop`, `u` and an `I` `pair`, and `Query.concrete` is a `T`, which
//! has `prop` and `u` and no implementations to choose between.
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
        pair: I
    }

    type A implements I {
        prop: String
        u(ctx: String): U
        pair: I
    }

    type B implements I {
        prop: String
        u(ctx: String): U
        pair: I
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

/// The test schema with `Query.media` returning an `A` alone, the plan's `media` returning nothing
/// else when read in it.
fn schema_with_media_a() -> ValidFederationSchema {
    let source = SCHEMA_STR.replace("media: I", "media: A");
    let schema = Schema::parse_and_validate(source, "schema.graphql").unwrap();
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

/// What a fetch at `path` needs of `available`, as the checker asks it, every field returning every
/// type it can, the fetch declaring its variable non-null.
fn context_met(
    path: &[FetchDataPathElement],
    condition: &[BooleanLiteral],
    available: &str,
    rewrites: Vec<Arc<FetchDataRewrite>>,
) -> bool {
    context_met_returning(path, condition, available, rewrites, &schema())
}

/// [`context_met`], with what the plan returns read in `returning`, a schema whose fields return
/// fewer types.
fn context_met_returning(
    path: &[FetchDataPathElement],
    condition: &[BooleanLiteral],
    available: &str,
    rewrites: Vec<Arc<FetchDataRewrite>>,
    returning: &ValidFederationSchema,
) -> bool {
    let schema = schema();
    let available = selections(&schema, available);
    let returned = ReturnedTypes::of_selections(&schema, returning, &name!("Query"), &available);
    context_met_given(path, condition, &available, rewrites, &returned)
}

/// [`context_met`], with what the plan returns read off the fetches of `returned`: every field
/// returning every type it can, but only fields `returned` selects.
fn context_met_selecting(
    path: &[FetchDataPathElement],
    condition: &[BooleanLiteral],
    available: &str,
    rewrites: Vec<Arc<FetchDataRewrite>>,
    returned: &str,
) -> bool {
    let schema = schema();
    let available = selections(&schema, available);
    let returned = selections(&schema, returned);
    let returned = ReturnedTypes::of_selections(&schema, &schema, &name!("Query"), &returned);
    context_met_given(path, condition, &available, rewrites, &returned)
}

fn context_met_given(
    path: &[FetchDataPathElement],
    condition: &[BooleanLiteral],
    available: &[Selection],
    rewrites: Vec<Arc<FetchDataRewrite>>,
    returned: &ReturnedTypes,
) -> bool {
    let schema = schema();
    // The variable is declared non-null, so the value is needed wherever the fetch runs, with no
    // contextual use to say where it is read.
    let operation = ExecutableDocument::parse_and_validate(
        schema.schema(),
        "query($contextual: String!) { concrete { u(ctx: $contextual) { id } } }",
        "fetch.graphql",
    )
    .unwrap();
    let fetch = FetchNode {
        subgraph_name: "s".into(),
        id: None,
        variable_usages: Vec::new(),
        requires: Vec::new(),
        operation_document: SerializableDocument::from_parsed(Arc::new(operation)),
        operation_name: None,
        operation_kind: executable::OperationType::Query,
        input_rewrites: Default::default(),
        output_rewrites: Vec::new(),
        context_rewrites: rewrites,
        protocol: Default::default(),
    };
    // Outside a deferred block, what a fetch can read is everything fetched before it. Every path
    // here ends at a `U`, so the `requires` is taken as written at `U`: written at another type, it
    // would leave the fetch running nowhere.
    check_context_rewrites(
        &schema,
        &name!("Query"),
        returned,
        &[],
        path,
        condition,
        available,
        available,
        &fetch,
        &[name!("U")],
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

/// Drop one of them and no rewrite targets a `B`: the router binds the variable to nothing there, so
/// no value is read, and availability asks for none. Whether one is owed is the declaration's
/// question (`one_setter_left_unread_is_rejected`).
#[test]
fn untargeted_runtime_type_is_owed_no_value() {
    assert!(context_met(
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

/// The same absent value, for a fetch whose path ends under a type condition no object there
/// matches: the fetch never runs, so it reads nothing.
#[test]
fn position_matching_no_object_demands_nothing() {
    let path = vec![
        key(name!("t")),
        key(name!("u")),
        FetchDataPathElement::AnyIndex(Some(vec![name!("I")])),
    ];
    assert!(context_met(
        &path,
        &[],
        "{ t { u { id } } }",
        vec![rewrite_on(name!("A")), rewrite_on(name!("B"))]
    ));
}

/// The value read through `media`, fetched only under `... on A`.
const MEDIA_NARROWED_TO_A: &str = "{ t { prop u { id } } media { ... on A { prop } } }";

fn read_media_prop() -> Arc<FetchDataRewrite> {
    renamer(vec![
        FetchDataPathElement::Parent,
        FetchDataPathElement::Parent,
        key(name!("media")),
        key(name!("prop")),
    ])
}

/// The value is narrowed to `A` past the levels the two paths share, and nothing about the fetch
/// says what that object is: a `B` there, which `media` can return, has no copy.
#[test]
fn narrowing_past_the_shared_levels_is_not_met() {
    assert!(!context_met(
        &fetch_path(),
        &[],
        MEDIA_NARROWED_TO_A,
        vec![read_media_prop()]
    ));
}

/// The value read through `media` and its `pair`, both of type `I`, fetched under a fragment per
/// type at each: copies narrowed apart at two levels. `pair_types` are the `pair` types fetched
/// below each `media` type.
fn media_pair_prop(pair_types: fn(&str) -> &'static [&'static str]) -> String {
    let media: Vec<String> = ["A", "B"]
        .iter()
        .map(|media_type| {
            let pairs: Vec<String> = pair_types(media_type)
                .iter()
                .map(|pair_type| format!("... on {pair_type} {{ prop }}"))
                .collect();
            format!("... on {media_type} {{ pair {{ {} }} }}", pairs.join(" "))
        })
        .collect();
    format!(
        "{{ t {{ prop u {{ id }} }} media {{ {} }} }}",
        media.join(" ")
    )
}

fn read_media_pair_prop() -> Arc<FetchDataRewrite> {
    renamer(vec![
        FetchDataPathElement::Parent,
        FetchDataPathElement::Parent,
        key(name!("media")),
        key(name!("pair")),
        key(name!("prop")),
    ])
}

/// Every pair of types the two levels can have has its copy, so the value is there whatever objects
/// the response holds.
#[test]
fn copies_narrowed_apart_at_two_levels_are_met() {
    assert!(context_met(
        &fetch_path(),
        &[],
        &media_pair_prop(|_| &["A", "B"]),
        vec![read_media_pair_prop()]
    ));
}

/// A `B` paired with a `B` has no copy.
#[test]
fn missing_pair_of_types_is_not_met() {
    assert!(!context_met(
        &fetch_path(),
        &[],
        &media_pair_prop(|media_type| if media_type == "A" {
            &["A", "B"]
        } else {
            &["A"]
        }),
        vec![read_media_pair_prop()]
    ));
}

/// The value narrowed to `A` past the shared levels, as in
/// [`narrowing_past_the_shared_levels_is_not_met`], where the plan's `media` returns nothing else:
/// no `B` can be there, so the copy serves every object that can.
#[test]
fn narrowing_to_every_returned_type_is_met() {
    assert!(context_met_returning(
        &fetch_path(),
        &[],
        MEDIA_NARROWED_TO_A,
        vec![read_media_prop()],
        &schema_with_media_a(),
    ));
}

/// `t`'s `pair` fetched for its `__typename` alone, as a fetch writes a field it can return no object
/// for.
const PAIR_TYPENAME_ONLY: &str = "{ t { pair { __typename } u { id } } }";
/// What the plan returns where no fetch returns an object for `pair`.
const RETURNING_NO_PAIR: &str = "{ t { u { id } } }";

fn read_pair_prop() -> Arc<FetchDataRewrite> {
    renamer(vec![
        FetchDataPathElement::Parent,
        key(name!("pair")),
        key(name!("prop")),
    ])
}

/// No fetch returns an object for `pair`, so it is null in every response, and the rewrite reading
/// `prop` below it finds no value, as the router then binds none: only `pair` itself is asked for.
#[test]
fn value_below_a_field_returning_no_object_is_owed_nothing() {
    assert!(context_met_selecting(
        &fetch_path(),
        &[],
        PAIR_TYPENAME_ONLY,
        vec![read_pair_prop()],
        RETURNING_NO_PAIR,
    ));
}

/// Where `pair` returns an `A` or a `B`, the `prop` below it is owed, and never fetched.
#[test]
fn value_below_a_field_returning_objects_is_not_met() {
    assert!(!context_met(
        &fetch_path(),
        &[],
        PAIR_TYPENAME_ONLY,
        vec![read_pair_prop()]
    ));
}

/// `pair` itself is still asked for: unfetched, the check cannot tell which field its key stands
/// for, nor whether that field returns an object.
#[test]
fn field_returning_no_object_not_fetched_is_not_met() {
    assert!(!context_met_selecting(
        &fetch_path(),
        &[],
        RETURNING_NO_PAIR,
        vec![read_pair_prop()],
        RETURNING_NO_PAIR,
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
