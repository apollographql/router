//! Cross-document sharing and mutation-isolation behavior.

use apollo_json::{NewValue, PathSegment, Value, ValueBuilder};

mod common;
use common::parse;

fn text(doc: &Value) -> String {
    doc.to_string()
}

/// Subtrees from three separately parsed documents composed into one
/// document; the sources are dropped and the composition alone keeps their
/// arenas alive and serializes all three subtrees.
#[test]
fn three_documents_compose_into_one() {
    let sources: Vec<Value> = (0..3)
        .map(|i| {
            parse(format!(
                r#"{{"data":{{"payload{i}":{{"id":{i},"tag":"src-{i}"}}}}}}"#
            ))
        })
        .collect();

    let mut builder = ValueBuilder::new();
    for (i, source) in sources.iter().enumerate() {
        let subtree = source
            .get("data")
            .and_then(|d| d.get(&format!("payload{i}")))
            .expect("payload exists");
        builder
            .set_path(
                &[
                    PathSegment::Key("data"),
                    PathSegment::Key(&format!("sub{i}")),
                ],
                NewValue::Node(subtree),
            )
            .expect("path resolves");
    }
    drop(sources);

    let composed = builder.seal();
    let out = text(&composed);
    for i in 0..3 {
        assert!(
            out.contains(&format!(r#""sub{i}":{{"id":{i},"tag":"src-{i}"}}"#)),
            "{out}"
        );
    }
}

/// A path walk crosses arena boundaries transparently: the leaf handle of a
/// path through an adopted subtree keeps the source arena alive on its own.
#[test]
fn get_path_resolves_through_a_composed_subtree() {
    let source = parse(r#"{"data":{"items":[{"id":7},{"id":8}]}}"#);
    let mut builder = ValueBuilder::new();
    builder
        .set("adopted", source.get("data").expect("present"))
        .expect("key applies");
    let composed = builder.seal();
    drop(source);

    let path: &[PathSegment<'_>] = &["adopted".into(), "items".into(), 1.into(), "id".into()];
    let leaf = composed.get_path(path).expect("path resolves");
    assert_eq!(
        composed.value().get_path(path).and_then(|v| v.as_i64()),
        Some(8)
    );
    drop(composed);
    // The owned leaf pins the source arena without the composition.
    assert_eq!(leaf.as_i64(), Some(8));
}

/// DAG semantics: one subtree referenced twice serializes twice.
#[test]
fn shared_subtree_expands_on_serialize() {
    let source = parse(r#"{"sub":{"k":"v"}}"#);
    let sub = source.get("sub").expect("sub exists");

    let mut builder = ValueBuilder::new();
    builder
        .set_path(&[PathSegment::Key("first")], NewValue::Node(sub.clone()))
        .unwrap();
    builder
        .set_path(&[PathSegment::Key("second")], NewValue::Node(sub))
        .unwrap();
    drop(source);

    let out = text(&builder.seal());
    assert_eq!(out.matches(r#"{"k":"v"}"#).count(), 2, "{out}");
}

/// Handles are `Send + Sync + 'static`: a subtree handle migrates to another
/// thread and reads and serializes there after the source document dropped.
#[test]
fn subtree_handle_crosses_threads() {
    let source = parse(r#"{"payload":{"n":42,"s":"moved"}}"#);
    let handle = source.get("payload").expect("payload exists");
    drop(source);

    let out = std::thread::spawn(move || {
        assert_eq!(handle.value().get("n").and_then(|v| v.as_i64()), Some(42));
        handle.to_vec()
    })
    .join()
    .expect("thread completes");
    assert_eq!(out, br#"{"n":42,"s":"moved"}"#);
}

/// Mutating inside an adopted subtree path-copies; the source document never
/// observes the change.
#[test]
fn mutation_in_composition_does_not_leak_into_source() {
    let source = parse(r#"{"shared":{"x":1,"y":2}}"#);

    let mut builder = ValueBuilder::new();
    builder
        .set_path(
            &[PathSegment::Key("adopted")],
            NewValue::Node(source.get("shared").unwrap()),
        )
        .unwrap();
    builder
        .set_path(
            &[PathSegment::Key("adopted"), PathSegment::Key("x")],
            NewValue::Int(99),
        )
        .unwrap();
    let composed = builder.seal();

    assert_eq!(
        source
            .value()
            .get("shared")
            .and_then(|s| s.get("x"))
            .and_then(|x| x.as_i64()),
        Some(1),
        "the source must not observe the composition's mutation"
    );
    let adopted = composed.value().get("adopted").expect("adopted exists");
    assert_eq!(adopted.get("x").and_then(|x| x.as_i64()), Some(99));
    assert_eq!(adopted.get("y").and_then(|y| y.as_i64()), Some(2));
}

/// Mutating the source after sharing: the composition's view is unaffected
/// (isolation in the other direction).
#[test]
fn mutating_source_does_not_leak_into_composition() {
    let source = parse(r#"{"shared":{"x":1,"y":2}}"#);

    let mut builder = ValueBuilder::new();
    builder
        .set_path(
            &[PathSegment::Key("adopted")],
            NewValue::Node(source.get("shared").unwrap()),
        )
        .unwrap();
    let composed = builder.seal();

    // The source arena is shared with the composition, so this builder takes
    // the copy-on-write path.
    let mut mutator = ValueBuilder::from_value(source);
    mutator
        .set_path(
            &[PathSegment::Key("shared"), PathSegment::Key("x")],
            NewValue::Int(42),
        )
        .unwrap();
    let mutated = mutator.seal();

    assert_eq!(
        mutated
            .value()
            .get("shared")
            .and_then(|s| s.get("x"))
            .and_then(|x| x.as_i64()),
        Some(42)
    );
    assert_eq!(
        composed
            .value()
            .get("adopted")
            .and_then(|s| s.get("x"))
            .and_then(|x| x.as_i64()),
        Some(1),
        "the composition must not observe the source's mutation"
    );
}

/// The execution-merge shape: chunks merged into a builder are adopted by
/// reference and the result matches a reference merge.
#[test]
fn merge_composes_chunks() {
    let chunks = [
        r#"{"data":{"_entities":[{"id":"1","a":1},{"id":"2","a":2}]}}"#,
        r#"{"data":{"_entities":[{"b":10},{"b":20}]}}"#,
        r#"{"data":{"extra":{"note":"n"}}}"#,
    ];
    let docs: Vec<Value> = chunks.iter().map(parse).collect();

    let mut builder = ValueBuilder::from_value(docs[0].clone());
    for doc in &docs[1..] {
        builder.merge(doc);
    }
    let merged = builder.seal();
    drop(docs);

    assert_eq!(
        text(&merged),
        r#"{"data":{"_entities":[{"id":"1","a":1,"b":10},{"id":"2","a":2,"b":20}],"extra":{"note":"n"}}}"#
    );
}

/// Sealing an untouched builder over a shared document yields the same
/// content without copying.
#[test]
fn untouched_builder_seals_to_the_same_document() {
    let doc = parse(r#"{"a":[1,2,3]}"#);
    let sealed = ValueBuilder::from_value(doc.clone()).seal();
    assert_eq!(text(&doc), text(&sealed));
}

/// `Hash` must agree with the order-independent object equality, including
/// for equal values assembled differently: reordered keys, composition
/// across arenas, and duplicate keys collapsed at parse time.
#[test]
fn equal_values_hash_equal_regardless_of_key_order_or_composition() {
    use std::hash::{BuildHasher, RandomState};

    let hasher = RandomState::new();
    let a = parse(r#"{"x":1,"y":{"b":2,"a":[1,"s"]}}"#);
    let b = parse(r#"{"y":{"a":[1,"s"],"b":2},"x":1}"#);
    assert_eq!(a, b);
    assert_eq!(
        hasher.hash_one(&a),
        hasher.hash_one(&b),
        "key order must not affect the hash"
    );

    let mut builder = ValueBuilder::new();
    builder.set("x", 1_i64).unwrap();
    builder.set("y", b.get("y").unwrap()).unwrap();
    let composed = builder.seal();
    assert_eq!(composed, a);
    assert_eq!(
        hasher.hash_one(&composed),
        hasher.hash_one(&a),
        "a composition equal to a parsed document must hash equal"
    );

    let collapsed = parse(r#"{"k":0,"k":7}"#);
    let plain = parse(r#"{"k":7}"#);
    assert_eq!(collapsed, plain);
    assert_eq!(hasher.hash_one(&collapsed), hasher.hash_one(&plain),);
}

/// `0` and `-0` compare equal (numbers compare as `f64`), so they must hash
/// equal — while the float reading itself keeps the sign.
#[test]
fn zero_and_negative_zero_hash_equal() {
    use std::hash::{BuildHasher, RandomState};

    let hasher = RandomState::new();
    let zero = parse("0");
    let negative_zero = parse("-0");
    assert_eq!(zero, negative_zero);
    assert_eq!(hasher.hash_one(&zero), hasher.hash_one(&negative_zero),);
    assert!(negative_zero.value().as_f64().unwrap().is_sign_negative());
}

/// Container hashes are framed by kind and length, so different nestings of
/// the same leaves do not collide structurally.
#[test]
fn distinct_nestings_of_the_same_leaves_hash_differently() {
    use std::hash::{BuildHasher, RandomState};

    let hasher = RandomState::new();
    let cases = [("[[1],[2]]", "[[1,2]]"), ("1", "[1]"), ("[]", "[[]]")];
    for (left, right) in cases {
        let left = parse(left);
        let right = parse(right);
        assert_ne!(left, right);
        assert_ne!(
            hasher.hash_one(&left),
            hasher.hash_one(&right),
            "{left:?} and {right:?} must not collide"
        );
    }
}

/// Opening a subtree for mutation must not copy it: the builder adopts the
/// source arena and path-copies only what it writes, so untouched siblings
/// stay shared with the original document.
#[test]
fn subtree_edit_shares_the_arena_and_copies_only_the_path() {
    let source = Value::parse(br#"{"keep":{"big":[1,2,3]},"edit":{"n":1}}"#.to_vec()).unwrap();
    let subtree = source.get("edit").unwrap();

    let mut builder = subtree.edit();
    builder.set("n", 2_i64).unwrap();
    let edited = builder.seal();

    assert_eq!(edited.to_vec(), br#"{"n":2}"#);
    // The source is untouched: mutation isolation holds across the adoption.
    assert_eq!(
        source.to_vec(),
        br#"{"keep":{"big":[1,2,3]},"edit":{"n":1}}"#
    );
    // Sharing the arena means the view is not self-contained until compacted.
    assert!(!source.get("edit").unwrap().is_self_contained());
}
