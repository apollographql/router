//! Builder mutation operations: remove, push, explicit set bounds, and
//! non-finite float rejection — including copy-on-write isolation when the
//! mutated document is shared.

use apollo_json::{JsonError, NewValue, PathSegment, Value, ValueBuilder};

mod common;
use common::parse;

fn text(doc: &Value) -> String {
    doc.to_string()
}

#[test]
fn remove_object_members_and_array_elements() {
    let doc = parse(r#"{"a":1,"b":[10,20,30],"c":{"x":true}}"#);
    let mut builder = ValueBuilder::from_value(doc);

    assert!(builder.remove_path(&[PathSegment::Key("a")]).unwrap());
    // Array removal shifts later elements left.
    assert!(
        builder
            .remove_path(&[PathSegment::Key("b"), PathSegment::Index(1)])
            .unwrap()
    );
    // Absent targets remove nothing.
    assert!(!builder.remove_path(&[PathSegment::Key("missing")]).unwrap());
    assert!(
        !builder
            .remove_path(&[PathSegment::Key("b"), PathSegment::Index(9)])
            .unwrap()
    );
    assert!(
        !builder
            .remove_path(&[PathSegment::Key("a"), PathSegment::Key("nested")])
            .unwrap(),
        "paths through removed or scalar values resolve to nothing"
    );

    assert_eq!(text(&builder.seal()), r#"{"b":[10,30],"c":{"x":true}}"#);
}

#[test]
fn remove_of_the_root_is_an_error() {
    let mut builder = ValueBuilder::from_value(parse(r#"{"a":1}"#));
    assert!(matches!(
        builder.remove_path(&[]),
        Err(JsonError::PathNotFound { segment: 0 })
    ));
}

#[test]
fn push_appends_to_arrays_only() {
    let mut builder = ValueBuilder::from_value(parse(r#"{"tags":["x"],"n":1}"#));
    builder
        .push_path(&[PathSegment::Key("tags")], NewValue::String("y".into()))
        .unwrap();
    builder
        .push_path(&[PathSegment::Key("tags")], NewValue::Null)
        .unwrap();
    assert!(matches!(
        builder.push_path(&[PathSegment::Key("n")], NewValue::Null),
        Err(JsonError::PathNotFound { .. })
    ));
    assert!(matches!(
        builder.push_path(&[PathSegment::Key("missing")], NewValue::Null),
        Err(JsonError::PathNotFound { .. })
    ));
    assert_eq!(text(&builder.seal()), r#"{"tags":["x","y",null],"n":1}"#);
}

/// `set` past the end of an array is an error; exactly at the end appends.
#[test]
fn set_beyond_array_length_is_an_error() {
    let mut builder = ValueBuilder::from_value(parse(r#"{"items":[1]}"#));
    builder
        .set_path(
            &[PathSegment::Key("items"), PathSegment::Index(1)],
            NewValue::Int(2),
        )
        .unwrap();
    assert!(matches!(
        builder.set_path(
            &[PathSegment::Key("items"), PathSegment::Index(5)],
            NewValue::Int(9),
        ),
        Err(JsonError::PathNotFound { segment: 1 })
    ));
    assert_eq!(text(&builder.seal()), r#"{"items":[1,2]}"#);
}

#[test]
fn non_finite_floats_are_rejected() {
    let mut builder = ValueBuilder::new();
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(matches!(
            builder.set_path(&[PathSegment::Key("x")], NewValue::Float(bad)),
            Err(JsonError::NonFiniteNumber)
        ));
        // Path errors take precedence: the array must exist first.
        assert!(matches!(
            builder.push_path(&[PathSegment::Key("missing")], NewValue::Float(bad)),
            Err(JsonError::PathNotFound { .. })
        ));
    }
    builder
        .set_path(&[PathSegment::Key("x")], NewValue::Float(1.5))
        .unwrap();
    assert_eq!(text(&builder.seal()), r#"{"x":1.5}"#);
}

/// Removing and pushing on a shared document copies on write: the original
/// never observes the mutation.
#[test]
fn remove_and_push_isolate_from_shared_documents() {
    let original = parse(r#"{"keep":{"a":1,"b":2},"list":[1,2,3]}"#);
    let before = text(&original);

    let mut builder = ValueBuilder::from_value(original.clone());
    builder
        .remove_path(&[PathSegment::Key("keep"), PathSegment::Key("a")])
        .unwrap();
    builder
        .remove_path(&[PathSegment::Key("list"), PathSegment::Index(0)])
        .unwrap();
    builder
        .push_path(&[PathSegment::Key("list")], NewValue::Int(4))
        .unwrap();
    let mutated = builder.seal();

    assert_eq!(text(&mutated), r#"{"keep":{"b":2},"list":[2,3,4]}"#);
    assert_eq!(text(&original), before, "the shared original is untouched");
}

/// Merge output interacts with removal: members merged in from another
/// document can be removed, and the source document is unaffected.
#[test]
fn remove_after_merge_leaves_the_source_untouched() {
    let base = parse(r#"{"data":{"a":1}}"#);
    let patch = parse(r#"{"data":{"b":{"x":1,"y":2}}}"#);
    let before = text(&patch);

    let mut builder = ValueBuilder::from_value(base);
    builder.merge(&patch);
    assert!(
        builder
            .remove_path(&[
                PathSegment::Key("data"),
                PathSegment::Key("b"),
                PathSegment::Key("x"),
            ])
            .unwrap()
    );
    let merged = builder.seal();

    assert_eq!(text(&merged), r#"{"data":{"a":1,"b":{"y":2}}}"#);
    assert_eq!(text(&patch), before, "the merged-in source is untouched");
}

/// Editing a wide object — past the width where the parser's duplicate-key
/// handling goes through a hashed index — keeps lookups correct through
/// growth, removal, and resealing.
#[test]
fn wide_object_lookups_survive_mutation() {
    let members: Vec<String> = (0..64).map(|i| format!(r#""key{i}":{i}"#)).collect();
    let doc = parse(format!("{{{}}}", members.join(",")));

    let mut builder = doc.edit();
    builder.set("key3", 333i64).unwrap(); // replace: same member set
    builder.set("added", 1000i64).unwrap(); // grow: opens the overlay
    assert!(builder.remove("key10"));
    let sealed = builder.seal();

    let root = sealed.value();
    assert_eq!(root.len(), Some(64));
    assert_eq!(root.get("key3").and_then(|v| v.as_i64()), Some(333));
    assert_eq!(root.get("added").and_then(|v| v.as_i64()), Some(1000));
    assert!(root.get("key10").is_none(), "removed member stays gone");
    assert_eq!(root.get("key63").and_then(|v| v.as_i64()), Some(63));
    assert_eq!(root.get("key11").and_then(|v| v.as_i64()), Some(11));
}

/// Optional Rust data writes without unwrapping: `Some` converts as its
/// content does and `None` writes `null`.
#[test]
fn options_write_their_content_or_null() {
    let mut builder = ValueBuilder::new();
    builder.set("some", Some(7i64)).unwrap();
    builder.set("none", None::<i64>).unwrap();
    builder.set("text", Some("kept")).unwrap();
    builder
        .set(
            "nested",
            NewValue::Array(vec![Some("x").into(), None::<&str>.into()]),
        )
        .unwrap();
    assert_eq!(
        text(&builder.seal()),
        r#"{"some":7,"none":null,"text":"kept","nested":["x",null]}"#
    );
}

/// A pending tree is written into one arena in a single pass: no intermediate
/// document per level, and the nested structure comes out in insertion order.
#[test]
fn pending_trees_are_written_in_one_pass() {
    let mut builder = ValueBuilder::new();
    builder
        .set(
            "data",
            NewValue::Object(vec![
                (
                    "users".into(),
                    NewValue::Array(vec![
                        NewValue::Object(vec![
                            ("name".into(), NewValue::String("bob".into())),
                            ("tags".into(), NewValue::Array(vec![])),
                        ]),
                        NewValue::Null,
                    ]),
                ),
                ("count".into(), NewValue::Int(2)),
            ]),
        )
        .unwrap();
    assert_eq!(
        text(&builder.seal()),
        r#"{"data":{"users":[{"name":"bob","tags":[]},null],"count":2}}"#
    );
}

/// A pending object follows the parser's duplicate-key rule: first position,
/// last value.
#[test]
fn pending_objects_keep_the_first_position_of_a_repeated_key() {
    let mut builder = ValueBuilder::new();
    builder
        .set(
            "x",
            NewValue::Object(vec![
                ("a".into(), NewValue::Int(1)),
                ("b".into(), NewValue::Int(2)),
                ("a".into(), NewValue::Int(3)),
            ]),
        )
        .unwrap();
    assert_eq!(text(&builder.seal()), r#"{"x":{"a":3,"b":2}}"#);
    // Same rule as parsing the equivalent input.
    assert_eq!(text(&parse(r#"{"a":1,"b":2,"a":3}"#)), r#"{"a":3,"b":2}"#);
}

/// An adopted handle nested inside a pending tree is still adopted by
/// reference, so a pending tree can splice existing subtrees into new
/// structure without copying them.
#[test]
fn pending_trees_adopt_nested_handles() {
    let source = parse(r#"{"kept":{"deep":[1,2,3]}}"#);
    let kept = source.get("kept").unwrap();

    let mut builder = ValueBuilder::new();
    builder
        .set(
            "wrapped",
            NewValue::Array(vec![NewValue::Object(vec![(
                "inner".into(),
                NewValue::Node(kept),
            )])]),
        )
        .unwrap();
    assert_eq!(
        text(&builder.seal()),
        r#"{"wrapped":[{"inner":{"deep":[1,2,3]}}]}"#
    );
}

/// Writing a non-finite float nested inside a pending tree is rejected like
/// any other write, rather than silently landing as something else.
#[test]
fn non_finite_floats_nested_in_a_pending_tree_are_rejected() {
    let mut builder = ValueBuilder::new();
    assert!(matches!(
        builder.set(
            "x",
            NewValue::Object(vec![(
                "deep".into(),
                NewValue::Array(vec![NewValue::Float(f64::NAN)]),
            )]),
        ),
        Err(JsonError::NonFiniteNumber)
    ));
}

/// The plain-data constructors coerce non-finite floats to `null` at every
/// depth, not only at the root of what they were handed.
#[test]
fn constructors_coerce_non_finite_floats_at_any_depth() {
    assert_eq!(Value::array([f64::NAN]).to_string(), "[null]");
    assert_eq!(
        Value::object([(
            "deep",
            NewValue::Array(vec![NewValue::Float(f64::INFINITY), NewValue::Float(1.5)]),
        )])
        .to_string(),
        r#"{"deep":[null,1.5]}"#
    );
}

/// A pending tree deep enough to matter is written without recursing, so the
/// arena write is bounded by the heap rather than the thread stack.
#[test]
fn deep_pending_trees_do_not_recurse_into_the_stack() {
    const DEPTH: usize = 50_000;
    let mut pending = NewValue::Int(1);
    for _ in 0..DEPTH {
        pending = NewValue::Array(vec![pending]);
    }
    let mut builder = ValueBuilder::new();
    builder.set("deep", pending).unwrap();
    let doc = builder.seal();
    // Serializing is a separate concern from writing; just confirm the write
    // completed and the shape survived at full depth.
    let mut cursor = doc.get("deep").unwrap();
    for _ in 0..DEPTH {
        cursor = cursor.index(0).expect("every level is a one-element array");
    }
    assert_eq!(cursor.as_i64(), Some(1));
}
